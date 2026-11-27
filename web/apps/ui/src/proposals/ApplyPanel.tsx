import type { Proposal, ProposalList } from "@print-partner/rust-client";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router";
import { proposalsQuery } from "../core/proposals";
import { writeCore } from "../core/rust";
import { reason } from "../core/writes";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { ScreenTemplate } from "../screen/ScreenTemplate";

const stateLabel: Record<Proposal["state"], string> = {
  pending: "Pending",
  applying: "Applying",
  expired: "Expired",
  unknown: "Outcome unknown — check the affected work before proposing again",
  invalidated_by_restore: "Invalidated by restore",
};
const date = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
const stamp = (value: string) => <time dateTime={value}>{date.format(new Date(value))}</time>;

type Choice = { action: Proposal; route: "applyProposal" | "dismissProposal" };

function Queue({
  list,
  disabled,
  choose,
}: {
  list: ProposalList;
  disabled: boolean;
  choose: (choice: Choice) => void;
}) {
  const byId = new Map(list.actions.map((action) => [action.id, action]));
  return (
    <>
      {list.omitted > 0 && <p>{list.omitted} older proposals are not shown.</p>}
      <ul className="pp-proposals">
        {list.actions.map((action) => {
          const waiting = action.depends_on.map((id) => byId.get(id)).find(Boolean);
          return (
            <li key={action.id} className="pp-proposal">
              <h2>{action.label}</h2>
              <p>{action.summary}</p>
              <p role="status">{stateLabel[action.state]}</p>
              <dl className="pp-proposal-meta">
                <dt>Staged by</dt>
                <dd>{action.staged_by}</dd>
                <dt>Created</dt>
                <dd>{stamp(action.created_at)}</dd>
                <dt>Expires</dt>
                <dd>{stamp(action.expires_at)}</dd>
                <dt>Depends on</dt>
                <dd>
                  {action.depends_on.map((id) => byId.get(id)?.label ?? id).join(", ") || "None"}
                </dd>
              </dl>
              {action.plan_id > 0 && (
                <Link to={`/builds/${action.plan_id}`}>Review Build {action.plan_id}</Link>
              )}
              {Object.keys(action.params).length > 0 && (
                <details>
                  <summary>Proposed changes</summary>
                  <pre>{JSON.stringify(action.params, null, 2)}</pre>
                </details>
              )}
              {action.state === "pending" && waiting && (
                <p>
                  Waiting for {waiting.label} ({stateLabel[waiting.state]}).
                </p>
              )}
              <div className="pp-dialog-actions">
                <button
                  type="button"
                  className="pp-button pp-button-primary"
                  disabled={disabled || action.state !== "pending" || waiting !== undefined}
                  onClick={() => choose({ action, route: "applyProposal" })}
                >
                  Apply
                </button>
                <button
                  type="button"
                  className="pp-button"
                  disabled={disabled || action.state === "applying"}
                  onClick={() => choose({ action, route: "dismissProposal" })}
                >
                  Dismiss
                </button>
              </div>
            </li>
          );
        })}
      </ul>
    </>
  );
}

export function ApplyPanel() {
  const connection = useConnection();
  const client = useQueryClient();
  const list = useQuery({ ...proposalsQuery, enabled: connection !== "offline" });
  const decision = useMutation({
    mutationFn: ({ action, route }: Choice) => writeCore(route, { id: action.id }),
    retry: false,
    onSettled: (_result, _error, { route }) =>
      client.invalidateQueries({
        queryKey: route === "applyProposal" ? ["core"] : proposalsQuery.queryKey,
      }),
  });
  return (
    <>
      {decision.isSuccess && (
        <p role="status" className="pp-banner">
          {decision.data === "applied" ? "Applied" : "Dismissed"}: {decision.variables.action.label}
        </p>
      )}
      {decision.isError && (
        <p role="alert" className="pp-banner" data-kind="error">
          {reason(decision.error)}
        </p>
      )}
      <ScreenTemplate
        title="Apply"
        state={queryScreenState(list, connection, ({ actions }) => actions.length === 0)}
        empty={{ message: "No proposals to review." }}
        renderData={(data) => (
          <Queue
            list={data}
            disabled={connection !== "online" || list.isError || decision.isPending}
            choose={decision.mutate}
          />
        )}
      />
    </>
  );
}
