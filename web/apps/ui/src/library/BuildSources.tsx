import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useRef, useState, type FormEvent } from "react";
import { useOutletContext } from "react-router";
import {
  BuildTabs,
  buildQuery,
  type BuildData,
  type WorkspaceContext,
} from "../builds/BuildWorkspace";
import { CoreError } from "../core/client";
import { writeCore } from "../core/rust";
import { sourcesQuery } from "../core/sources";
import { OFFLINE_REASON, useConnection } from "../frame/connection";
import { queryScreenState } from "../screen/queryState";
import { NotFound } from "../screen/screens";
import { ScreenTemplate } from "../screen/ScreenTemplate";
import { refusalText, SourceSelect } from "../screen/SourceSelect";
import { LineCell } from "./LibraryScreen";
import { SourceFiles } from "./SourceFiles";
import { acceptedRevisionText, revisionText, type Source } from "./sources";

/** Each Source once, in layer order. A layer with no Source is skipped. */
function sourcesInLayerOrder(
  layers: readonly {
    layer_order: number;
    project_id: number | null;
    project_name: string | null;
  }[],
): { id: number; name: string }[] {
  const seen = new Set<number>();
  const sources: { id: number; name: string }[] = [];
  for (const layer of [...layers].sort((a, b) => a.layer_order - b.layer_order)) {
    if (layer.project_id == null || seen.has(layer.project_id)) continue;
    seen.add(layer.project_id);
    sources.push({ id: layer.project_id, name: layer.project_name ?? "Source" });
  }
  return sources;
}

/** Where the Build's re-read after a send stands. Set base Source waits for it. */
type Reread = "idle" | "reading" | "failed";

/**
 * A Build with no layers: choose a Library Source as its base. Set base Source sends
 * `setBuildBase` once, then stays closed until the Build has been read again: a second
 * send would replace a base the first one already set. A failed re-read keeps it closed
 * and offers Read again.
 */
function SetBaseSource({ buildId }: { buildId: number }) {
  const client = useQueryClient();
  const offline = useConnection() === "offline";
  const ids = useId();
  const [sourceId, setSourceId] = useState<number | null>(null);
  const [failure, setFailure] = useState("");
  const [reread, setReread] = useState<Reread>("idle");
  const sending = useRef(false);
  const readAgain = async () => {
    setReread("reading");
    const queryKey = buildQuery(buildId).queryKey;
    await client.invalidateQueries({ queryKey });
    setReread(client.getQueryState(queryKey)?.status === "error" ? "failed" : "idle");
    sending.current = false;
  };
  const set = useMutation({
    retry: false,
    networkMode: "always",
    mutationFn: (projectId: number) =>
      writeCore("setBuildBase", { id: buildId }, { project_id: projectId }),
    // Runs before the mutation stops pending, so the form never reopens on old layers.
    onSettled: () => {
      void readAgain();
      void client.invalidateQueries({ queryKey: ["core", "workflow", buildId] });
      void client.invalidateQueries({ queryKey: ["core", "tasks"] });
    },
    onError: (error: Error) => {
      const refused = error instanceof CoreError && error.status < 500;
      setFailure(
        refused
          ? refusalText(error.detail ?? error.message)
          : `Could not confirm the base Source was set: ${error.message}. The Build is read again; check it before you try again.`,
      );
    },
  });
  const why = offline
    ? OFFLINE_REASON
    : set.isPending
      ? "Setting the base Source."
      : reread === "reading"
        ? "Reading the Build again."
        : reread === "failed"
          ? "Could not read the Build again. Press Read again to see whether the base Source was set."
          : sourceId === null
            ? "Choose a Source."
            : null;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (why !== null || sourceId === null || sending.current) return;
    sending.current = true;
    setFailure("");
    set.mutate(sourceId);
  };
  return (
    <form onSubmit={submit} noValidate aria-label="Set base Source">
      <p>This Build has no layers, so it uses no Sources. Choose its base Source to plan it.</p>
      <div className="pp-fields">
        <div className="pp-field">
          <label htmlFor={`${ids}-source`}>Base Source</label>
          <SourceSelect
            id={`${ids}-source`}
            value={sourceId}
            allowNone={false}
            onChange={(next) => {
              setFailure("");
              setSourceId(next);
            }}
          />
        </div>
      </div>
      {failure && (
        <p role="alert" className="pp-field-error">
          {failure}
        </p>
      )}
      <div className="pp-dialog-actions">
        <button
          type="submit"
          className="pp-button pp-button-primary"
          aria-disabled={why !== null || undefined}
          aria-describedby={why ? `${ids}-why` : undefined}
        >
          Set base Source
        </button>
        {reread === "failed" && (
          <button type="button" className="pp-button" onClick={() => void readAgain()}>
            Read again
          </button>
        )}
        <span id={`${ids}-why`} role="status" className="pp-mutation-reason">
          {why}
        </span>
      </div>
    </form>
  );
}

function SourcesBody({ build, layers }: BuildData) {
  const sources = useQuery(sourcesQuery);
  const byId = new Map(sources.data?.sources.map((source) => [source.id, source]));
  const layered = [...layers].sort((a, b) => a.layer_order - b.layer_order);
  const fileSources = sourcesInLayerOrder(layered);
  if (layered.length === 0) return <SetBaseSource buildId={build.id} />;
  const sourceCells = (source: Source | undefined) => {
    if (!source) {
      const why = sources.isError
        ? `Could not load: ${sources.error.message}`
        : sources.data
          ? "Not in the Library"
          : "Loading";
      return (
        <td colSpan={2}>
          <span className="pp-muted">{why}</span>
        </td>
      );
    }
    return (
      <>
        <td>
          <LineCell line={revisionText(source)} />
        </td>
        <td>
          <LineCell line={acceptedRevisionText(build.freshness, source)} />
        </td>
      </>
    );
  };
  return (
    <>
      {sources.isError && (
        <div role="alert" className="pp-banner" data-kind="error">
          <p>Could not load the Library&apos;s Sources.</p>
          <p className="pp-banner-detail">{sources.error.message}</p>
          <button type="button" className="pp-button" onClick={() => void sources.refetch()}>
            Retry
          </button>
        </div>
      )}
      <table className="pp-sources-table" aria-label="Build Sources">
        <thead>
          <tr>
            <th scope="col">Layer</th>
            <th scope="col">Source</th>
            <th scope="col">Current revision</th>
            <th scope="col">Accepted Plan uses</th>
          </tr>
        </thead>
        <tbody>
          {layered.map((layer) => (
            <tr key={layer.id}>
              <td>{layer.layer_type}</td>
              <th scope="row">{layer.project_name ?? "No Source"}</th>
              {!layer.project_id ? (
                <td colSpan={2}>
                  <span className="pp-muted">No Source on this layer</span>
                </td>
              ) : (
                sourceCells(byId.get(layer.project_id))
              )}
            </tr>
          ))}
        </tbody>
      </table>
      {fileSources.map((source) => (
        <SourceFiles key={source.id} sourceId={source.id} sourceName={source.name} />
      ))}
    </>
  );
}

/** The Sources tab: each layer's Source and revision, then that Source's files and STL thumbnails. */
export function BuildSources() {
  const { id } = useOutletContext<WorkspaceContext>();
  const connection = useConnection();
  const query = useQuery(buildQuery(id));
  if (query.error instanceof CoreError && query.error.status === 404 && !query.data) {
    return <NotFound />;
  }
  return (
    <ScreenTemplate
      title="Sources"
      subnav={<BuildTabs id={id} />}
      state={queryScreenState(query, connection)}
      renderData={(data) => <SourcesBody {...data} />}
      empty={{ message: "" }}
    />
  );
}
