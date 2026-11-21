import { useQuery, type UseQueryResult } from "@tanstack/react-query";
import { useId } from "react";
import { useParams } from "react-router";
import { dateText } from "../builds/format";
import { sourcesQuery } from "../core/sources";
import { useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { ScreenTemplate } from "../screen/ScreenTemplate";
import { AddSource } from "./AddSourceDialog";
import { LibraryFilesNext } from "./FilesNext";
import { SourceFiles } from "./SourceFiles";
import { selectedSourceId } from "./sourceFilesModel";
import {
  ACTIVITY_LIMIT,
  eventText,
  latestEvents,
  sourceActivityQuery,
  type Line,
  type Source,
  type SourceEvent,
} from "./sources";
import { SourceRows, useLibraryJobs } from "./ZipUpload";

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
  none,
}: {
  events: Map<number, SourceEvent> | undefined;
  id: number;
  none: string;
}) {
  if (!events) return <span className="pp-muted">Not loaded</span>;
  const event = events.get(id);
  if (!event) return <span className="pp-muted">{none}</span>;
  return <LineCell line={{ label: eventText(event), detail: dateText(event.at) }} />;
}

function SourcesTable({
  sources,
  events,
  noActivity,
  jobs,
  selectedId,
}: {
  sources: Source[];
  events: Map<number, SourceEvent> | undefined;
  noActivity: string;
  jobs: ReturnType<typeof useLibraryJobs>;
  selectedId: number | null;
}) {
  return (
    <table className="pp-sources-table" aria-label="Sources">
      <thead>
        <tr>
          <th scope="col">Source</th>
          <th scope="col">Revision</th>
          <th scope="col">Sync</th>
          <th scope="col">Latest activity</th>
          <th scope="col">Upload</th>
        </tr>
      </thead>
      <tbody>
        {sources.map((source) => (
          <SourceRows
            key={source.id}
            source={source}
            activity={<ActivityCell events={events} id={source.id} none={noActivity} />}
            jobId={jobs.jobs[String(source.id)]}
            note={jobs.refusals[String(source.id)]}
            remember={jobs.remember}
            forget={jobs.forget}
            selected={source.id === selectedId}
          />
        ))}
      </tbody>
    </table>
  );
}

/**
 * With no events, names the row control that fits the Library: Sync when any Source is a
 * GitHub Source, otherwise Upload ZIP. It does not promise the action records activity.
 */
function RecentActivity({ events, hasGithub }: { events: SourceEvent[]; hasGithub: boolean }) {
  const headingId = useId();
  return (
    <section aria-labelledby={headingId}>
      <h2 id={headingId}>Recent activity</h2>
      {events.length === 0 ? (
        <p className="pp-muted">
          {hasGithub
            ? "No Source activity yet. To bring a GitHub Source up to date, press Sync on its row."
            : "No Source activity yet. To add or replace a Source's files, press Upload ZIP on its row."}
        </p>
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

/** Library: every Source, and the files of the one selected at `/library/:sourceId`. */
export function LibraryScreen() {
  const connection = useConnection();
  const sources = useQuery(sourcesQuery);
  const activity = useQuery(sourceActivityQuery);
  const jobs = useLibraryJobs();
  const { sourceId } = useParams();
  const selectedId = selectedSourceId(sourceId);
  const state = queryScreenState(sources, connection, (data) => data.sources.length === 0);
  const events = activity.data?.events;
  return (
    <ScreenTemplate
      title="Library"
      // Under the heading in every state, so an open dialog survives the screen going
      // empty, offline or back.
      subnav={
        <p className="pp-screen-actions">
          <AddSource onJob={jobs.remember} onRefused={jobs.refuse} />
        </p>
      }
      state={state}
      renderData={(data) => {
        const selected = data.sources.find((source) => source.id === selectedId);
        return (
          <>
            <ActivityNotice query={activity} />
            {selected ? (
              <SourceFiles
                key={selected.id}
                sourceId={selected.id}
                sourceName={selected.name}
                next={
                  <LibraryFilesNext
                    sourceId={selected.id}
                    kind={selected.source_kind}
                    running={jobs.jobs[String(selected.id)] != null}
                  />
                }
              />
            ) : (
              selectedId != null && <p>That Source is not in the Library.</p>
            )}
            <SourcesTable
              sources={data.sources}
              jobs={jobs}
              selectedId={selectedId}
              events={events && latestEvents(events)}
              // Fewer events than the limit means the core returned all of them.
              noActivity={
                (events?.length ?? 0) < ACTIVITY_LIMIT
                  ? "None recorded"
                  : `None in the last ${ACTIVITY_LIMIT} events`
              }
            />
            {events && (
              <RecentActivity
                events={events}
                hasGithub={data.sources.some((source) => source.source_kind === "github")}
              />
            )}
          </>
        );
      }}
      empty={{
        message:
          "No Sources yet. Press Add Source to add a kit from a ZIP archive or a GitHub repository.",
      }}
    />
  );
}
