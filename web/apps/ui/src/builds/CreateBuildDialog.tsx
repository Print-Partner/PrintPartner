import {
  createBuildRequestSchema,
  type CreateBuildRequest,
  type ProfileList,
  type ProfileSummary,
} from "@print-partner/rust-client";
import { useMutation, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useId, useRef, useState, type FormEvent } from "react";
import { Link, useNavigate } from "react-router";
import { buildsQuery } from "../core/builds";
import { CoreError } from "../core/client";
import { RequestContractError, writeCore } from "../core/rust";
import { sourcesQuery } from "../core/sources";
import { OFFLINE_REASON, useConnection } from "../frame/connection";
import { Dialog } from "../frame/Dialog";
import { refusalText, SourceSelect } from "../screen/SourceSelect";

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

/**
 * Statuses the create handler refuses with before or inside its one write transaction, so
 * nothing of that request commits later. Any other answer may hide a created Build.
 */
const REFUSALS = new Set([400, 401, 403, 409, 413]);

function refused(error: Error): error is CoreError | RequestContractError {
  if (error instanceof RequestContractError) return true;
  return error instanceof CoreError && error.problem && REFUSALS.has(error.status);
}

/** What the dialog says after Create, when it stays open. */
type Outcome =
  | { kind: "refused"; text: string }
  | { kind: "checking" }
  | { kind: "found"; build: ProfileSummary; created: boolean }
  | { kind: "absent" }
  | { kind: "unread" };

/** The list's Build with exactly this name; names compare exactly, as the core does. */
async function readNamed(client: QueryClient, name: string): Promise<ProfileSummary | undefined> {
  const list = await client.fetchQuery({ ...buildsQuery, staleTime: 0 });
  return list.profiles.find((build) => build.name === name);
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
 * A name and a base Source. Create sends `createBuild` once and never again by itself. After an
 * answer that may hide a created Build, it reads the Builds list and looks for the name before
 * Create is offered again; the core also refuses a name already in use, so no press can add a
 * second Build. A created Build opens on its Overview.
 */
function CreateBuildDialog({ onClose }: { onClose: () => void }) {
  const client = useQueryClient();
  const navigate = useNavigate();
  const offline = useConnection() === "offline";
  const ids = useId();
  const sources = useQuery(sourcesQuery);
  const [name, setName] = useState("");
  // Undefined until the user picks, so the default follows the Sources as they load.
  const [picked, setPicked] = useState<number | null | undefined>(undefined);
  // The first Source is the default base, and `null` only once the list has loaded empty.
  // Undefined while the list loads or after it failed: Create waits rather than send `null`.
  const sourceId =
    picked === undefined ? sources.data && (sources.data.sources[0]?.id ?? null) : picked;
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const sending = useRef(false);
  const create = useMutation({
    retry: false,
    networkMode: "always",
    mutationFn: (body: CreateBuildRequest) => writeCore("createBuild", {}, body),
    // Runs even after the dialog closed: the list is read again whatever the answer.
    onSettled: () => {
      void client.invalidateQueries({ queryKey: buildsQuery.queryKey });
      void client.invalidateQueries({ queryKey: ["core", "tasks"] });
    },
  });
  const checking = outcome?.kind === "checking";
  const why = offline
    ? OFFLINE_REASON
    : create.isPending
      ? "Creating the Build."
      : checking
        ? "Checking the Builds list."
        : (nameProblem(name) ??
          (sourceId !== undefined
            ? null
            : sources.isError && !sources.isFetching
              ? "The Library's Sources did not load. Press Try again."
              : "Waiting for the Library's Sources."));
  const blocked = why !== null;
  // `before` holds the Build ids listed before the send; null when the core refused the name.
  const settle = (sent: string, before: ReadonlySet<number> | null) => {
    setOutcome({ kind: "checking" });
    readNamed(client, sent)
      .then(
        (build) =>
          setOutcome(
            build
              ? { kind: "found", build, created: before !== null && !before.has(build.id) }
              : { kind: "absent" },
          ),
        () => setOutcome({ kind: "unread" }),
      )
      .finally(() => {
        sending.current = false;
      });
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (blocked || sourceId === undefined || sending.current) return;
    sending.current = true;
    setOutcome(null);
    const body = buildRequest(name, sourceId);
    const listed = client.getQueryData<ProfileList>(buildsQuery.queryKey)?.profiles ?? [];
    const before = new Set(listed.map((build) => build.id));
    // Per-call callbacks do not run once the dialog has closed, so a late answer never
    // navigates away from a screen the user has moved on to.
    create.mutate(body, {
      onSuccess: (build) => {
        sending.current = false;
        onClose();
        void navigate(`/builds/${build.id}`);
      },
      onError: (error: Error) => {
        if (!refused(error)) return settle(body.name, before);
        const detail = (error instanceof CoreError && error.detail) || error.message;
        // A name in use may be the Build an earlier, unconfirmed press created: name it.
        if (detail.startsWith("Profile already exists")) return settle(body.name, null);
        sending.current = false;
        setOutcome({ kind: "refused", text: createRefusalText(detail) });
      },
    });
  };
  return (
    <Dialog title="Create Build" onClose={onClose}>
      <form onSubmit={submit} noValidate>
        <div className="pp-fields">
          <div className="pp-field">
            <label htmlFor={`${ids}-name`}>Name</label>
            <input
              id={`${ids}-name`}
              value={name}
              autoComplete="off"
              onChange={(event) => {
                if (!checking) setOutcome(null);
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
                if (!checking) setOutcome(null);
                setPicked(next);
              }}
            />
          </div>
        </div>
        {outcome && outcome.kind !== "checking" && <OutcomeText outcome={outcome} />}
        <div className="pp-dialog-actions">
          <button type="button" className="pp-button" onClick={onClose}>
            {create.isPending ? "Close" : "Cancel"}
          </button>
          <button
            type="submit"
            className="pp-button pp-button-primary"
            aria-disabled={blocked || undefined}
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

/** The core's create refusals in the UI's words; the core still says "Profile" for a Build. */
function createRefusalText(detail: string): string {
  if (detail.startsWith("Profile name is")) return detail.replace("Profile", "Build");
  return refusalText(detail);
}

/** The dialog's line about the last Create, once nothing is in flight. */
function outcomeText(outcome: Exclude<Outcome, { kind: "checking" }>): string {
  switch (outcome.kind) {
    case "refused":
      return outcome.text;
    case "found":
      return outcome.created
        ? "The answer to Create was lost, but the Builds list now holds this Build, so it was created."
        : "A Build with this name already exists. Open it, or choose another name.";
    case "absent":
      return "Could not confirm the Build was created, and the Builds list holds no Build with this name. Pressing Create again is safe: the core refuses a second Build with the same name.";
    case "unread":
      return "Could not confirm the Build was created, and the Builds list did not load. Check Builds before you press Create again.";
  }
}

function OutcomeText({ outcome }: { outcome: Exclude<Outcome, { kind: "checking" }> }) {
  return (
    <p role="alert" className="pp-field-error">
      {outcomeText(outcome)}
      {outcome.kind === "found" && (
        <>
          {" "}
          <Link to={`/builds/${outcome.build.id}`} className="pp-link">
            Open {outcome.build.name}
          </Link>
        </>
      )}
    </p>
  );
}
