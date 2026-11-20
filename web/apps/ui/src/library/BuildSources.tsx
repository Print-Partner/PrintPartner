import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useId, useState, type FormEvent } from "react";
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
import {
  clearUnconfirmed,
  markUnconfirmed,
  useUnconfirmedBase,
  type UnconfirmedBase,
} from "./unconfirmedBase";

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

/** A `setBuildBase` for this Build that is sending, or reading the Build again after it. */
const setBaseKey = (buildId: number) => ["core", "setBuildBase", buildId] as const;

const READING = "Reading the Build again.";

const LATE_SEND =
  "If that send reaches the core later, whichever of the two arrives last becomes the base and replaces the other.";

/**
 * Sends `setBuildBase` once. From the press until the core confirms or refuses it, the send is
 * recorded as unconfirmed (see `unconfirmedBase.ts`); a 5xx or lost answer leaves it there.
 */
function useSetBase(buildId: number) {
  const client = useQueryClient();
  const [refusal, setRefusal] = useState("");
  const mutation = useMutation({
    mutationKey: setBaseKey(buildId),
    retry: false,
    networkMode: "always",
    mutationFn: (projectId: number) =>
      writeCore("setBuildBase", { id: buildId }, { project_id: projectId }),
    onMutate: (projectId) => {
      markUnconfirmed(client, buildId, { sourceId: projectId, reason: null });
    },
    // The answer is the Build's layers now: show them at once, so the form closes for good.
    onSuccess: (answer) => {
      clearUnconfirmed(client, buildId);
      client.setQueryData(buildQuery(buildId).queryKey, (old: BuildData | undefined) =>
        old ? { ...old, layers: answer.layers } : old,
      );
    },
    onError: (error: Error, projectId) => {
      if (error instanceof CoreError && error.status < 500) {
        clearUnconfirmed(client, buildId);
        setRefusal(refusalText(error.detail ?? error.message));
      } else {
        markUnconfirmed(client, buildId, { sourceId: projectId, reason: error.message });
      }
    },
    // Returned, so the send stays in flight until the Build has been read again.
    onSettled: () =>
      Promise.all([
        client.invalidateQueries({ queryKey: buildQuery(buildId).queryKey, exact: true }),
        client.invalidateQueries({ queryKey: ["core", "workflow", buildId] }),
        client.invalidateQueries({ queryKey: ["core", "tasks"] }),
      ]),
  });
  const send = (sourceId: number) => {
    // Asked at the press, not taken from the last render, so two forms cannot both send.
    if (client.isMutating({ mutationKey: setBaseKey(buildId), exact: true }) > 0) return;
    setRefusal("");
    mutation.mutate(sourceId);
  };
  return { send, refusal, clearRefusal: () => setRefusal("") };
}

/**
 * A Build with no layers: choose a Library Source as its base. The core's PUT moves an existing
 * base, so a second send would replace the first. The plain form opens only on layers read
 * after it mounted, with no send for this Build in flight and none unconfirmed. An unconfirmed
 * send shows `UnconfirmedBase` instead, whose Set a base anyway is the only way to send again.
 */
function SetBaseSource({ buildId }: { buildId: number }) {
  const client = useQueryClient();
  const build = useQuery(buildQuery(buildId));
  // The layers this form mounted on may predate a base set before it mounted: read them again.
  // A read already in flight is kept, so a send's re-read is not cancelled and left unsettled.
  useEffect(() => {
    void client.invalidateQueries(
      { queryKey: buildQuery(buildId).queryKey, exact: true },
      { cancelRefetch: false },
    );
  }, [client, buildId]);
  const sending = useIsMutating({ mutationKey: setBaseKey(buildId), exact: true }) > 0;
  const unconfirmed = useUnconfirmedBase(buildId);
  const { send, refusal, clearRefusal } = useSetBase(buildId);
  const [anyway, setAnyway] = useState(false);
  const read = build.isFetchedAfterMount && !build.isFetching;
  const reading = !read
    ? READING
    : build.isError
      ? "Could not read the Build again. Press Read again to see whether the base Source was set."
      : null;
  const readAgain = () => void build.refetch();
  if (unconfirmed && !sending && !anyway) {
    return (
      <UnconfirmedSend
        unconfirmed={unconfirmed}
        reading={reading}
        readAgain={readAgain}
        setAnyway={() => setAnyway(true)}
      />
    );
  }
  return (
    <BaseForm
      unconfirmed={anyway ? unconfirmed : null}
      sending={sending}
      reading={reading}
      readAgain={readAgain}
      refusal={refusal}
      clearRefusal={clearRefusal}
      send={(sourceId) => {
        setAnyway(false);
        send(sourceId);
      }}
      cancel={() => setAnyway(false)}
    />
  );
}

function useSourceName(sourceId: number): string {
  const sources = useQuery(sourcesQuery);
  return sources.data?.sources.find((source) => source.id === sourceId)?.name ?? "that Source";
}

/** An unconfirmed send: no plain form, only Read again and the explicit Set a base anyway. */
function UnconfirmedSend({
  unconfirmed,
  reading,
  readAgain,
  setAnyway,
}: {
  unconfirmed: UnconfirmedBase;
  reading: string | null;
  readAgain: () => void;
  setAnyway: () => void;
}) {
  const offline = useConnection() === "offline";
  const ids = useId();
  const name = useSourceName(unconfirmed.sourceId);
  const why = offline ? OFFLINE_REASON : reading;
  return (
    <section aria-label="Unconfirmed base Source">
      <p role="alert" className="pp-field-error">
        Could not confirm the base Source was set: {unconfirmed.reason ?? "no answer"}. The core
        may still set {name} as this Build&apos;s base, so Set base Source stays closed until a
        read shows a base.
      </p>
      <div className="pp-dialog-actions">
        <button
          type="button"
          className="pp-button"
          aria-disabled={reading === READING || undefined}
          onClick={() => reading !== READING && readAgain()}
        >
          Read again
        </button>
        <button
          type="button"
          className="pp-button"
          aria-disabled={why === null ? undefined : true}
          aria-describedby={why ? `${ids}-why` : undefined}
          onClick={() => why === null && setAnyway()}
        >
          Set a base anyway
        </button>
        <span id={`${ids}-why`} role="status" className="pp-mutation-reason">
          {why ?? "The last read shows no base Source yet."}
        </span>
      </div>
    </section>
  );
}

/**
 * Choose a Source and send it. With `unconfirmed` it is Set a base anyway: it says what the
 * earlier send may still do, and the Source must be chosen again.
 */
function BaseForm({
  unconfirmed,
  sending,
  reading,
  readAgain,
  refusal,
  clearRefusal,
  send,
  cancel,
}: {
  unconfirmed: UnconfirmedBase | null;
  sending: boolean;
  reading: string | null;
  readAgain: () => void;
  refusal: string;
  clearRefusal: () => void;
  send: (sourceId: number) => void;
  cancel: () => void;
}) {
  const offline = useConnection() === "offline";
  const ids = useId();
  const [sourceId, setSourceId] = useState<number | null>(null);
  const earlier = useSourceName(unconfirmed?.sourceId ?? 0);
  const label = unconfirmed ? "Set base anyway" : "Set base Source";
  const why = offline
    ? OFFLINE_REASON
    : sending
      ? "Setting the base Source."
      : (reading ?? (sourceId === null ? "Choose a Source." : null));
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (why === null && sourceId !== null) send(sourceId);
  };
  return (
    <form onSubmit={submit} noValidate aria-label={label}>
      {unconfirmed ? (
        <p>
          The earlier send of {earlier} was not confirmed. {LATE_SEND} Choose the Source again to
          set it now.
        </p>
      ) : (
        <p>This Build has no layers, so it uses no Sources. Choose its base Source to plan it.</p>
      )}
      <div className="pp-fields">
        <div className="pp-field">
          <label htmlFor={`${ids}-source`}>Base Source</label>
          <SourceSelect
            id={`${ids}-source`}
            value={sourceId}
            onChange={(next) => {
              clearRefusal();
              setSourceId(next);
            }}
          />
        </div>
      </div>
      {refusal && (
        <p role="alert" className="pp-field-error">
          {refusal}
        </p>
      )}
      <div className="pp-dialog-actions">
        <button
          type="submit"
          className="pp-button pp-button-primary"
          aria-disabled={why === null ? undefined : true}
          aria-describedby={why ? `${ids}-why` : undefined}
        >
          {label}
        </button>
        {reading !== null && reading !== READING && (
          <button type="button" className="pp-button" onClick={readAgain}>
            Read again
          </button>
        )}
        {unconfirmed && (
          <button type="button" className="pp-button" onClick={cancel}>
            Cancel
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
  const client = useQueryClient();
  const sources = useQuery(sourcesQuery);
  // A read that shows a base settles any unconfirmed send: whichever send it was, it arrived.
  const based = layers.length > 0;
  useEffect(() => {
    if (based) clearUnconfirmed(client, build.id);
  }, [client, build.id, based]);
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
