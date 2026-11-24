import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router";
import { buildsQuery } from "../core/builds";
import { SENDING_POLL_MS, sendQueueQuery, sendStatus } from "../core/sendQueue";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { ScreenTemplate } from "../screen/ScreenTemplate";

export function FarmSendQueue({
  printers,
}: {
  printers: readonly { id: string; name: string }[];
}) {
  const connection = useConnection();
  const queue = useQuery({
    ...sendQueueQuery,
    select: ({ items }) => items.filter((item) => item.state !== "cancelled"),
    refetchInterval: ({ state }) =>
      state.data?.items.some((item) => item.state === "sending") ? SENDING_POLL_MS : false,
  });
  const builds = useQuery({
    ...buildsQuery,
    enabled: queue.data?.some((item) => item.profile_id !== null) ?? false,
  });
  const buildNames = new Map(builds.data?.profiles.map(({ id, name }) => [id, name]));
  const printerNames = new Map(printers.map(({ id, name }) => [id, name]));

  return (
    <ScreenTemplate
      title="Send queue"
      heading="h2"
      state={queryScreenState(queue, connection, (items) => items.length === 0)}
      empty={{ message: "No sends in the queue." }}
      renderData={(items) => (
        <table className="pp-farm-table" aria-label="Send queue" aria-busy={builds.isLoading || undefined}>
          <thead>
            <tr>
              <th scope="col">Build</th>
              <th scope="col">File</th>
              <th scope="col">Printer</th>
              <th scope="col">State</th>
            </tr>
          </thead>
          <tbody>
            {items.map((item) => {
              const build = item.profile_id;
              const name = build === null ? "No Build" : (buildNames.get(build) ?? `Build #${build}`);
              return (
                <tr key={item.id}>
                  <th scope="row">
                    {name}
                    {build !== null && (
                      <span className="pp-fact-detail">
                        <Link
                          to={`/builds/${build}/production`}
                          className="pp-link"
                          aria-label={`Open Production for ${name}`}
                        >
                          Production
                        </Link>
                      </span>
                    )}
                  </th>
                  <td>{item.filename}</td>
                  <td>{printerNames.get(item.printer_id) ?? item.printer_id}</td>
                  <td>{sendStatus(item)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    />
  );
}
