import type { CreateBuildRequest } from "@print-partner/rust-client";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useId, useRef, useState, type FormEvent } from "react";
import { useNavigate } from "react-router";
import { CoreError } from "../core/client";
import { writeCore } from "../core/rust";
import { OFFLINE_REASON, useConnection } from "../frame/connection";
import { Dialog } from "../frame/Dialog";
import { sourcesQuery } from "../core/sources";
import { refusalText, SourceSelect } from "../screen/SourceSelect";

/** The `createBuild` body: the trimmed name, and the base Source when one is chosen. */
export function buildRequest(name: string, sourceId: number | null): CreateBuildRequest {
  return { name: name.trim(), base_project_id: sourceId };
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
 * A name and a base Source. Create sends `createBuild` once and never again by itself; the core
 * refuses a name already in use, so pressing Create after a lost answer cannot add a second
 * Build. A created Build opens on its Overview.
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
  const [failure, setFailure] = useState("");
  const sending = useRef(false);
  const create = useMutation({
    retry: false,
    networkMode: "always",
    mutationFn: (body: CreateBuildRequest) => writeCore("createBuild", {}, body),
    // Runs even after the dialog closed: the list is read again whatever the answer.
    onSettled: () => {
      sending.current = false;
      void client.invalidateQueries({ queryKey: ["core", "builds"] });
      void client.invalidateQueries({ queryKey: ["core", "tasks"] });
    },
  });
  const why = offline
    ? OFFLINE_REASON
    : create.isPending
      ? "Creating the Build."
      : !name.trim()
        ? "Enter a name."
        : sourceId !== undefined
          ? null
          : sources.isError
            ? "The Library's Sources did not load. Close and open Create Build to try again."
            : "Waiting for the Library's Sources.";
  const blocked = why !== null || create.isPending;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (blocked || sourceId === undefined || sending.current) return;
    sending.current = true;
    setFailure("");
    // Per-call callbacks do not run once the dialog has closed, so a late answer never
    // navigates away from a screen the user has moved on to.
    create.mutate(buildRequest(name, sourceId), {
      onSuccess: (build) => {
        onClose();
        void navigate(`/builds/${build.id}`);
      },
      onError: (error: Error) => {
        // A 4xx is the core's refusal; anything else may have created the Build.
        const refused = error instanceof CoreError && error.status < 500;
        setFailure(
          refused
            ? refusalText(error.detail ?? error.message)
            : `Could not confirm the Build was created: ${error.message}. The list is read again; check it before you press Create again.`,
        );
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
                setFailure("");
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
                setFailure("");
                setPicked(next);
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
