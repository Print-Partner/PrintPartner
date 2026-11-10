import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import { useId } from "react";
import { dateText } from "../builds/format";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { ScreenTemplate } from "../screen/ScreenTemplate";
import {
  ACTIVITY_LIMIT,
  eventText,
  latestEvents,
  revisionText,
  sourceActivityQuery,
  sourcesQuery,
  syncText,
  type Line,
  type Source,
  type SourceEvent,
} from "./sources";

export function LineCell({ line }: { line: Line }) {
  return (
    <>
      {line.label}
      {line.detail && <span className="pp-fact-detail">{line.detail}</span>}
    </>
  );
}

/** Says when Source activity could not load, with a Retry. The Sources still show. */
function ActivityNotice({ query }: { query: UseQueryResult<unknown> }) {
  if (!query.isError) return null;
  return (
    <div role="alert" className="pp-banner" data-kind="error">
      <p>
        {query.data === undefined
          ? "Could not load Source activity."
          : "Could not refresh Source activity."}
      </p>
      <p className="pp-banner-detail">{query.error.message}</p>
      <button type="button" className="pp-button" onClick={() => void query.refetch()}>
        Retry
      </button>
    </div>
  );
}

function ActivityCell({
  events,
  id,
}: {
  events: Map<number, SourceEvent> | undefined;
  id: number;
}) {
  if (!events) return <span className="pp-muted">Not loaded</span>;
  const event = events.get(id);
  if (!event)
    return <span className="pp-muted">{`None in the last ${ACTIVITY_LIMIT} events`}</span>;
  return <LineCell line={{ label: eventText(event), detail: dateText(event.at) }} />;
}

function SourcesTable({
  sources,
  events,
}: {
  sources: Source[];
  events: Map<number, SourceEvent> | undefined;
}) {
  return (
    <table className="pp-sources-table" aria-label="Sources">
      <thead>
        <tr>
          <th scope="col">Source</th>
          <th scope="col">Revision</th>
          <th scope="col">Sync</th>
          <th scope="col">Latest activity</th>
        </tr>
      </thead>
      <tbody>
        {sources.map((source) => (
          <tr key={source.id}>
            <th scope="row">
              {source.name}
              {!source.content_available && <span className="pp-tag">No files on disk</span>}
              <span className="pp-fact-detail">
                {[source.source_type, source.category].filter(Boolean).join(", ")}
              </span>
            </th>
            <td>
              <LineCell line={revisionText(source)} />
            </td>
            <td>
              <LineCell line={syncText(source)} />
            </td>
            <td>
              <ActivityCell events={events} id={source.id} />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function RecentActivity({ events }: { events: SourceEvent[] }) {
  const headingId = useId();
  return (
    <section aria-labelledby={headingId}>
      <h2 id={headingId}>Recent activity</h2>
      {events.length === 0 ? (
        <p className="pp-muted">No Source activity yet.</p>
      ) : (
        <ul className="pp-reasons">
          {events.map((event) => (
            <li key={event.id}>
              {event.source_name}: {eventText(event)}{" "}
              <span className="pp-muted">{dateText(event.at)}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** Library: every Source with its current revision, sync state and latest activity. Read only. */
export function LibraryScreen() {
  const connection = useConnection();
  const sources = useQuery(sourcesQuery);
  const activity = useQuery(sourceActivityQuery);
  const state = queryScreenState(sources, connection, (data) => data.sources.length === 0);
  const events = activity.data?.events;
  return (
    <ScreenTemplate
      title="Library"
      state={state}
      renderData={(data) => (
        <>
          <ActivityNotice query={activity} />
          <SourcesTable sources={data.sources} events={events && latestEvents(events)} />
          {events && <RecentActivity events={events} />}
        </>
      )}
      empty={{ message: "No Sources yet. Sources you add appear here." }}
    />
  );
}
