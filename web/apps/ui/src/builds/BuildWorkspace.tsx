import type { ProfileLayers, ProfileSummary } from "@print-partner/rust-client";
import { useQuery } from "@tanstack/react-query";
import { useId, type ReactNode } from "react";
import { NavLink, Outlet, useOutletContext, useParams } from "react-router";
import { CoreError } from "../core/client";
import { useModule } from "../core/modules";
import { readCore } from "../core/rust";
import { buildWorkflowQuery, openDecisionCount } from "../core/workflow";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { NotFound } from "../screen/screens";
import { ModuleOff, ScreenTemplate } from "../screen/ScreenTemplate";
import { dateText, freshnessText, progressText } from "./format";

/** The Build workspace tabs from docs/product/ui-ux-target.md. The Machine tab is parked. */
export const BUILD_TABS = [
  { label: "Overview", path: "", holds: "" },
  { label: "Sources", path: "sources", holds: "the Sources this Build is built from" },
  { label: "Plan", path: "plan", holds: "the accepted Plan and its drafts" },
  { label: "Production", path: "production", holds: "plates and sends" },
  { label: "Checkoff", path: "checkoff", holds: "what came off each printer" },
  { label: "History", path: "history", holds: "accepted revisions, snapshots and checkoff bursts" },
] as const;

interface BuildData {
  build: ProfileSummary;
  layers: ProfileLayers["layers"];
}

type WorkspaceContext = { id: number };

/** A positive safe-integer Build ID from the address, or null. */
function buildId(raw: string | undefined): number | null {
  if (!raw || !/^[1-9]\d*$/.test(raw)) return null;
  const id = Number(raw);
  return Number.isSafeInteger(id) ? id : null;
}

const buildQuery = (id: number) => ({
  queryKey: ["core", "build", id],
  queryFn: async ({ signal }: { signal: AbortSignal }): Promise<BuildData> => {
    const [build, layers] = await Promise.all([
      readCore("readBuild", { id }, signal),
      readCore("listBuildLayers", { id }, signal),
    ]);
    return { build, layers: layers.layers };
  },
  retry: (failures: number, error: Error) =>
    !(error instanceof CoreError && error.status === 404) && failures < 3,
});

/** `/builds/:id` and its tabs. Each tab renders the tab bar under its heading. */
export function BuildWorkspace() {
  const id = buildId(useParams().id);
  if (id === null) return <NotFound />;
  return <Outlet context={{ id } satisfies WorkspaceContext} />;
}

function BuildTabs({ id }: { id: number }) {
  return (
    <nav aria-label="Build sections" className="pp-tabs">
      <ul>
        {BUILD_TABS.map((tab) => (
          <li key={tab.label}>
            <NavLink to={`/builds/${id}${tab.path && `/${tab.path}`}`} end className="pp-tab">
              {tab.label}
            </NavLink>
          </li>
        ))}
      </ul>
    </nav>
  );
}

/** The projection's next action and open-task count, or why they are missing. */
function WorkflowFacts({ id }: { id: number }) {
  const query = useQuery(buildWorkflowQuery(id));
  const workflow = query.data;
  if (!workflow) {
    const detail = query.isError ? (
      <>
        <span className="pp-muted">Could not load: {query.error.message}</span>{" "}
        <button type="button" className="pp-button" onClick={() => void query.refetch()}>
          Retry
        </button>
      </>
    ) : (
      <span className="pp-muted">Loading</span>
    );
    return <Fact term="Next action">{detail}</Fact>;
  }
  return (
    <>
      <Fact term="Next action">
        {workflow.next_action.label}
        <span className="pp-fact-detail">{workflow.next_action.reason}</span>
      </Fact>
      <Fact term="Open decisions">
        <span className="num">{openDecisionCount(workflow)}</span>
      </Fact>
    </>
  );
}

function Fact({ term, children }: { term: string; children: ReactNode }) {
  return (
    <div className="pp-fact">
      <dt>{term}</dt>
      <dd>{children}</dd>
    </div>
  );
}

function OverviewBody({ build, layers }: BuildData) {
  const orders = useModule("orders");
  const layersId = useId();
  const freshness = freshnessText(build.freshness);
  return (
    <>
      <dl className="pp-facts">
        <Fact term="Progress">{progressText(build.accepted_progress)}</Fact>
        <Fact term="Parts">
          <span className="num">{build.part_count}</span>
        </Fact>
        <Fact term="Plan">
          {freshness.headline}
          {freshness.reasons.length > 0 && (
            <ul className="pp-reasons">
              {freshness.reasons.map((reason) => (
                <li key={reason}>{reason}</li>
              ))}
            </ul>
          )}
        </Fact>
        <WorkflowFacts id={build.id} />
        {orders?.state === "on" && (
          <>
            <Fact term="Order">
              <span className="num">{build.order_number ?? "None"}</span>
            </Fact>
            <Fact term="Customer request">{build.special_request ?? "None"}</Fact>
          </>
        )}
        <Fact term="Last used">{dateText(build.last_used_at) ?? "Never"}</Fact>
        {build.archived_at && <Fact term="Archived">{dateText(build.archived_at)}</Fact>}
      </dl>
      {orders?.state === "off" && (
        <ModuleOff
          module="Orders"
          wouldShow="this Build's order number and customer request"
          where="the Overview"
        />
      )}
      <h2 id={layersId}>Layers</h2>
      {layers.length === 0 ? (
        <p>This Build has no layers.</p>
      ) : (
        <ol className="pp-layers" aria-labelledby={layersId}>
          {[...layers]
            .sort((a, b) => a.layer_order - b.layer_order)
            .map((layer) => (
              <li key={layer.id}>
                <span>{layer.project_name ?? "No Source"}</span>{" "}
                <span className="pp-muted">{layer.layer_type}</span>
              </li>
            ))}
        </ol>
      )}
    </>
  );
}

/** The Overview tab: what the Build is and how far along, read only. */
export function BuildOverview() {
  const { id } = useOutletContext<WorkspaceContext>();
  const connection = useConnection();
  const query = useQuery(buildQuery(id));
  if (query.error instanceof CoreError && query.error.status === 404 && !query.data) {
    return <NotFound />;
  }
  const name = query.data?.build.name ?? "Build";
  return (
    <ScreenTemplate
      title={name}
      subnav={<BuildTabs id={id} />}
      state={queryScreenState(query, connection)}
      renderData={(data) => <OverviewBody {...data} />}
      empty={{ message: "" }}
    />
  );
}

/** A tab whose slice has not landed: says what it will hold. */
export function LaterTab({ label, holds }: { label: string; holds: string }) {
  const { id } = useOutletContext<WorkspaceContext>();
  const offline = useConnection() === "offline";
  return (
    <ScreenTemplate
      title={label}
      subnav={<BuildTabs id={id} />}
      state={offline ? { kind: "offline" } : { kind: "empty" }}
      renderData={() => null}
      empty={{ message: `This tab arrives in a later slice. It will hold ${holds}.` }}
    />
  );
}
