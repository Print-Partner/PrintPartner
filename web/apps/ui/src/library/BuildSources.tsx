import { useIsMutating, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useId, useState, type FormEvent } from "react";
import { useOutletContext } from "react-router";
import {
  BuildTabs,
  buildQuery,
  type BuildData,
  type WorkspaceContext,
} from "../builds/BuildWorkspace";
import { routePath, routes, type ProfileLayers } from "@print-partner/rust-client";
import { CoreError, fetchCore } from "../core/client";
import { ContractError, RequestContractError } from "../core/rust";
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
  baseSeen,
  recordSend,
  sendRefused,
  sendUnconfirmed,
  unconfirmedSends,
  useUnconfirmedSends,
  type UnconfirmedSend,
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
  "That send can still reach the core and replace the base you set now, and the base you set now can replace one it already set.";

/**
 * Statuses the core's own set-base handler refuses with, writing nothing
 * (`set_base` in `rust/crates/pp-api/src/builds/http.rs`, through `attachment`, `receive`,
 * `present` and `domain_error`): 400 an unreadable body, or a Source the Build cannot use
 * ("Project not found"); 401 no valid session; 403 the account needs an owner mapping; 404 no
 * such Build, no such Source, or the Source is already on the Build; 413 a body over the limit.
 * Each is decided before or inside the one write transaction, so nothing of that request can
 * commit after it. Its 503s (writer queue full, writer stopped, outcome unknown) and 500s
 * can follow a write that still commits, so they are not refusals.
 */
const CORE_REFUSALS = new Set([400, 401, 403, 404, 413]);

/** Only the core's own problem with a refusal status. A proxy's 4xx page is not one. */
const refusedByCore = (error: Error) =>
  error instanceof CoreError && error.problem && CORE_REFUSALS.has(error.status);

/**
 * One PUT. A redirect is refused, not followed: a followed 307 or 308 would send the PUT again,
 * and the destination's answer is not the core's. The refused redirect is a network error, so
 * the send stays unconfirmed.
 */
async function putBase(buildId: number, projectId: number): Promise<ProfileLayers> {
  const path = routePath("setBuildBase", { id: buildId });
  const request = routes.setBuildBase.request.safeParse({ project_id: projectId });
  if (!request.success) throw new RequestContractError(path);
  const response = await fetchCore(path, {
    method: "PUT",
    headers: { accept: "application/json", "content-type": "application/json" },
    body: JSON.stringify(request.data),
    redirect: "error",
  });
  const parsed = routes.setBuildBase.response.safeParse(await response.json().catch(() => undefined));
  if (!parsed.success) throw new ContractError(path);
  return parsed.data;
}

/**
 * Sends `setBuildBase` once. The send is recorded before it leaves (see `unconfirmedBase.ts`)
 * and stays recorded until the core confirms it or refuses it with its own problem.
 */
function useSetBase(buildId: number) {
  const client = useQueryClient();
  const [refusal, setRefusal] = useState("");
  const mutation = useMutation({
    mutationKey: setBaseKey(buildId),
    retry: false,
    networkMode: "always",
    mutationFn: ({ sourceId }: { sourceId: number; sendId: number }) => putBase(buildId, sourceId),
    // The answer is the Build's layers now: show them at once, so the form closes for good.
    onSuccess: (answer) => {
      baseSeen(buildId);
      client.setQueryData(buildQuery(buildId).queryKey, (old: BuildData | undefined) =>
        old ? { ...old, layers: answer.layers } : old,
      );
    },
    onError: (error: Error, { sendId }) => {
      if (refusedByCore(error)) {
        sendRefused(buildId, sendId);
        setRefusal(refusalText((error as CoreError).detail ?? error.message));
      } else if (error instanceof RequestContractError) {
        // Refused before it left: nothing was sent.
        sendRefused(buildId, sendId);
        setRefusal(error.message);
      } else {
        sendUnconfirmed(buildId, sendId, error.message);
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
  /** `anyway` is the owner's explicit Set a base anyway; the plain form never passes it. */
  const send = (sourceId: number, anyway: boolean) => {
    // Asked at the press, not taken from the last render, so two forms cannot both send.
    if (client.isMutating({ mutationKey: setBaseKey(buildId), exact: true }) > 0) return;
    if (!anyway && unconfirmedSends(buildId).length > 0) return;
    setRefusal("");
    mutation.mutate({ sourceId, sendId: recordSend(buildId, sourceId) });
  };
  return { send, refusal, clearRefusal: () => setRefusal("") };
}

/**
 * A Build with no layers: choose a Library Source as its base. The core's PUT moves an existing
 * base, so a second send would replace the first. The plain form opens only on layers read
 * after it mounted, with no send for this Build in flight and none unconfirmed. An unconfirmed
 * send shows `UnconfirmedPanel` instead, whose Set a base anyway is the only way to send again.
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
  const unconfirmed = useUnconfirmedSends(buildId).at(-1) ?? null;
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
      <UnconfirmedPanel
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
        send(sourceId, anyway);
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
function UnconfirmedPanel({
  unconfirmed,
  reading,
  readAgain,
  setAnyway,
}: {
  unconfirmed: UnconfirmedSend;
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
  unconfirmed: UnconfirmedSend | null;
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
          set a base now.
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
  const sources = useQuery(sourcesQuery);
  // A read that shows a base ends the record: no form shows while the Build has a base.
  const based = layers.length > 0;
  useEffect(() => {
    if (based) baseSeen(build.id);
  }, [build.id, based]);
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
