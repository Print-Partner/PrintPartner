import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useOutletContext } from "react-router";
import { useModule } from "../core/modules";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { MutationButton, ScreenTemplate } from "../screen/ScreenTemplate";
import { BuildTabs, type WorkspaceContext } from "./BuildWorkspace";
import { SendRowActions } from "./SendRowActions";
import { SendSheet, type SendOutcome } from "./SendSheet";
import {
  buildSends,
  SENDING_POLL_MS,
  sendPrintersQuery,
  sendQueueQuery,
  sendStatus,
  type QueuedSend,
  type SendPrinter,
} from "./sends";

function When({ iso }: { iso: string | null }) {
  if (!iso) return <>Unknown</>;
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return <>{iso}</>;
  const text = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
  return <time dateTime={date.toISOString()}>{text.format(date)}</time>;
}

function SendRows({
  sends,
  printers,
}: {
  sends: QueuedSend[];
  printers: Map<string, SendPrinter>;
}) {
  return (
    <table className="pp-farm-table" aria-label="Sends">
      <thead>
        <tr>
          <th scope="col">File</th>
          <th scope="col">Printer</th>
          <th scope="col">What happens</th>
          <th scope="col">Time</th>
          <th scope="col">State</th>
          <th scope="col">Actions</th>
        </tr>
      </thead>
      <tbody>
        {sends.map((item) => {
          const status = sendStatus(item);
          return (
            <tr key={item.id}>
              <td>{item.filename}</td>
              <td>{printers.get(item.printer_id)?.name ?? item.printer_id}</td>
              <td>{item.start ? "Upload and start print" : "Upload only"}</td>
              <td>
                <When iso={item.updated_at ?? item.created_at} />
              </td>
              <td>
                {status}
                {status === "Failed" && item.error && (
                  <span className="pp-fact-detail">{item.error}</span>
                )}
              </td>
              <td>
                <SendRowActions item={item} printer={printers.get(item.printer_id)} />
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

function OutcomeNotice({ outcome }: { outcome: SendOutcome | null }) {
  if (!outcome || outcome.kind === "sent") return null;
  return (
    <div
      role="status"
      className="pp-banner"
      data-kind={outcome.kind === "lost" ? "stale" : "error"}
    >
      {outcome.kind === "lost" ? (
        <p>
          Print Partner could not confirm what the core did, so the list below shows what the core
          has. Nothing was sent again.
        </p>
      ) : (
        <p>Not sent: {outcome.reason}</p>
      )}
    </div>
  );
}

/**
 * The Production tab: this Build's sends and "Send a file". Nothing is sent when the tab
 * opens or reloads; only the sheet's Send or the confirmation's Start print sends.
 */
export function BuildProduction() {
  const { id } = useOutletContext<WorkspaceContext>();
  const connection = useConnection();
  const farm = useModule("farm");
  const [sheetOpen, setSheetOpen] = useState(false);
  const [outcome, setOutcome] = useState<SendOutcome | null>(null);
  const queue = useQuery({
    ...sendQueueQuery,
    enabled: farm?.state === "on",
    select: (data) => buildSends(data, id),
    refetchInterval: (query) =>
      query.state.data && buildSends(query.state.data, id).some((item) => item.state === "sending")
        ? SENDING_POLL_MS
        : false,
  });
  const printers = useQuery({ ...sendPrintersQuery, enabled: farm?.state === "on" });
  const byId = new Map((printers.data?.printers ?? []).map((p) => [p.id, p]));

  const sendButton = (
    <MutationButton
      primary
      onClick={() => {
        setOutcome(null);
        setSheetOpen(true);
      }}
    >
      Send a file
    </MutationButton>
  );
  const state =
    farm?.state === "off"
      ? ({
          kind: "module-off",
          module: "Farm",
          wouldShow: "this Build's sends and a sheet to send a sliced file to a printer",
        } as const)
      : farm === undefined
        ? ({ kind: "loading" } as const)
        : queryScreenState(queue, connection, (sends) => sends.length === 0);
  return (
    <>
      <ScreenTemplate
        title="Production"
        subnav={<BuildTabs id={id} />}
        state={state}
        renderData={(sends) => (
          <>
            <OutcomeNotice outcome={outcome} />
            {sendButton}
            <SendRows sends={sends} printers={byId} />
          </>
        )}
        empty={{
          message: "No sends for this Build yet.",
          action: (
            <>
              <OutcomeNotice outcome={outcome} />
              {sendButton}
            </>
          ),
        }}
      />
      {sheetOpen && farm?.state === "on" && (
        <SendSheet
          build={id}
          onClose={(result) => {
            setSheetOpen(false);
            if (result) setOutcome(result);
          }}
        />
      )}
    </>
  );
}
