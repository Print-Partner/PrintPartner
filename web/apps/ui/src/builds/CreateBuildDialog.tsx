import { createBuildRequestSchema, type CreateBuildRequest } from "@print-partner/rust-client";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useId, useState, useSyncExternalStore, type FormEvent } from "react";
import { Link, useNavigate } from "react-router";
import { sourcesQuery } from "../core/sources";
import { OFFLINE_REASON, useConnection } from "../frame/connection";
import { Dialog } from "../frame/Dialog";
import { SourceSelect } from "../screen/SourceSelect";
import { busy, createStore, startCreate, type CreateState } from "./createBuild";

/** The `createBuild` body: the trimmed name, and the base Source when one is chosen. */
export function buildRequest(name: string, sourceId: number | null): CreateBuildRequest {
  return { name: name.trim(), base_project_id: sourceId };
}

/** The longest Build name the core stores, in characters (`BuildName`). */
const NAME_MAX = 4096;

/** Why the core would refuse this name before it is sent, or null when it fits `BuildName`. */
export function nameProblem(name: string): string | null {
  const trimmed = name.trim();
  if (!trimmed) return "Enter a name.";
  if (createBuildRequestSchema.shape.name.safeParse(trimmed).success) return null;
  return [...trimmed].length > NAME_MAX
    ? `Shorten the name to ${NAME_MAX.toLocaleString("en-US")} characters or fewer.`
    : "The name has a character the core cannot store. Retype it.";
}

/** Builds' "Create Build" button and its dialog. */
export function CreateBuild() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" className="pp-button pp-button-primary" onClick={() => setOpen(true)}>
        Create Build
      </button>
      {open && <CreateBuildDialog onClose={() => setOpen(false)} />}
    </>
  );
}

/**
 * A name and a base Source. Create sends `createBuild` once and never again by itself; the
 * create and its check live in `createBuild.ts`, so closing the dialog keeps them. After an
 * answer that may hide a created Build, the Builds list is read and the name looked for before
 * Create is offered again. A created Build opens on its Overview.
 */
function CreateBuildDialog({ onClose }: { onClose: () => void }) {
  const client = useQueryClient();
  const navigate = useNavigate();
  const offline = useConnection() === "offline";
  const ids = useId();
  const sources = useQuery(sourcesQuery);
  const create = useSyncExternalStore(createStore.subscribe, createStore.get);
  // Reopened while a create is busy or after it settled: show the name it sent.
  const [name, setName] = useState(() => ("name" in create ? create.name : ""));
  // Undefined until the user picks, so the default follows the Sources as they load.
  const [picked, setPicked] = useState<number | null | undefined>(undefined);
  // The first Source is the default base, and `null` only once the list has loaded empty.
  // Undefined while the list loads or after it failed: Create waits rather than send `null`.
  const sourceId =
    picked === undefined ? sources.data && (sources.data.sources[0]?.id ?? null) : picked;
  useEffect(() => createStore.mount(), []);
  const close = () => {
    createStore.clear();
    onClose();
  };
  useEffect(() => {
    if (create.kind !== "opened") return;
    createStore.clear();
    onClose();
    void navigate(`/builds/${create.build.id}`);
  }, [create, navigate, onClose]);
  const why = offline
    ? OFFLINE_REASON
    : create.kind === "sending"
      ? "Creating the Build."
      : create.kind === "checking"
        ? "Checking the Builds list."
        : (nameProblem(name) ??
          (sourceId !== undefined
            ? null
            : sources.isError && !sources.isFetching
              ? "The Library's Sources did not load. Press Try again."
              : "Waiting for the Library's Sources."));
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (why !== null || sourceId === undefined) return;
    void startCreate(client, buildRequest(name, sourceId));
  };
  return (
    <Dialog title="Create Build" onClose={close}>
      <form onSubmit={submit} noValidate>
        <div className="pp-fields">
          <div className="pp-field">
            <label htmlFor={`${ids}-name`}>Name</label>
            <input
              id={`${ids}-name`}
              value={name}
              autoComplete="off"
              onChange={(event) => {
                createStore.clear();
                setName(event.target.value);
              }}
            />
          </div>
          <div className="pp-field">
            <label htmlFor={`${ids}-source`}>Base Source</label>
            <SourceSelect
              id={`${ids}-source`}
              value={sourceId ?? null}
              allowNone
              onChange={(next) => {
                createStore.clear();
                setPicked(next);
              }}
            />
          </div>
        </div>
        <OutcomeText state={create} />
        <div className="pp-dialog-actions">
          <button type="button" className="pp-button" onClick={close}>
            {busy(create) ? "Close" : "Cancel"}
          </button>
          <button
            type="submit"
            className="pp-button pp-button-primary"
            aria-disabled={why !== null || undefined}
            aria-describedby={why ? `${ids}-why` : undefined}
          >
            Create
          </button>
          {/* Mounted while empty, so a screen reader announces each new reason. */}
          <span id={`${ids}-why`} role="status" className="pp-mutation-reason">
            {why}
          </span>
        </div>
      </form>
    </Dialog>
  );
}

/** The dialog's line about the last Create once it settled, or null while none has. */
function outcomeText(state: CreateState): string | null {
  switch (state.kind) {
    case "refused":
      return state.text;
    case "listed":
      return {
        created:
          "The answer to Create was lost, but the Builds list now holds this Build, so it was created.",
        unproven:
          "Could not confirm the Build was created, and the Builds list holds a Build with this name that may be an earlier one. Open it, or choose another name.",
        taken: "A Build with this name already exists. Open it, or choose another name.",
      }[state.why];
    case "absent":
      return "Could not confirm the Build was created, and the Builds list holds no Build with this name. Pressing Create again is safe: the core refuses a second Build with the same name.";
    case "unread":
      return "Could not confirm the Build was created, and the Builds list did not load. Check Builds before you press Create again.";
    default:
      return null;
  }
}

function OutcomeText({ state }: { state: CreateState }) {
  const text = outcomeText(state);
  if (text === null) return null;
  return (
    <p role="alert" className="pp-field-error">
      {text}
      {state.kind === "listed" && (
        <>
          {" "}
          <Link
            to={`/builds/${state.build.id}`}
            className="pp-link"
            onClick={() => createStore.clear()}
          >
            Open {state.build.name}
          </Link>
        </>
      )}
    </p>
  );
}
