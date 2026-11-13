import type { CreateSourceRequest } from "@print-partner/rust-client";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useId, useRef, useState, type FormEvent } from "react";
import { CoreError } from "../core/client";
import { writeCore } from "../core/rust";
import { OFFLINE_REASON, useConnection } from "../frame/connection";
import { Dialog } from "../frame/Dialog";
import { sourcesQuery } from "./sources";

export type SourceKind = "github" | "archive";

export interface SourceForm {
  name: string;
  kind: SourceKind;
  url: string;
  branch: string;
}

const EMPTY: SourceForm = { name: "", kind: "archive", url: "", branch: "main" };

/** Why Create cannot send yet, or null when the form is complete. */
export function missing(form: SourceForm): string | null {
  if (!form.name.trim()) return "Enter a name.";
  if (form.kind === "github" && !form.url.trim()) return "Enter the repository URL.";
  return null;
}

/** The `createSource` body: a ZIP Source is a name only; GitHub adds its URL and branch. */
export function sourceRequest(form: SourceForm): CreateSourceRequest {
  const name = form.name.trim();
  if (form.kind === "archive") return { name, source_kind: "archive" };
  const branch = form.branch.trim();
  return { name, source_kind: "github", url: form.url.trim(), ...(branch && { branch }) };
}

/**
 * Library's "Add Source": a name and a kind, ZIP archive or GitHub repository. Create sends
 * `createSource` once and never again by itself; the core refuses a name already in use, so
 * pressing Create after a lost answer cannot add a second Source.
 */
export function AddSource() {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" className="pp-button pp-button-primary" onClick={() => setOpen(true)}>
        Add Source
      </button>
      {open && <AddSourceDialog onClose={() => setOpen(false)} />}
    </>
  );
}

function AddSourceDialog({ onClose }: { onClose: () => void }) {
  const client = useQueryClient();
  const offline = useConnection() === "offline";
  const ids = useId();
  const [form, setForm] = useState<SourceForm>(EMPTY);
  const [failure, setFailure] = useState("");
  const sending = useRef(false);
  const create = useMutation({
    retry: false,
    networkMode: "always",
    mutationFn: (body: CreateSourceRequest) => writeCore("createSource", {}, body),
    onSuccess: () => onClose(),
    onError: (error: Error) => {
      // A 4xx is the core's refusal; anything else may have added the Source.
      const refused = error instanceof CoreError && error.status < 500;
      setFailure(
        refused
          ? (error.detail ?? error.message)
          : `Could not confirm the Source was added: ${error.message}. The list is read again; check it before you press Create again.`,
      );
    },
    onSettled: () => {
      sending.current = false;
      void client.invalidateQueries({ queryKey: sourcesQuery.queryKey });
    },
  });
  const why = offline ? OFFLINE_REASON : missing(form);
  const blocked = why !== null || create.isPending;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (blocked || sending.current) return;
    sending.current = true;
    setFailure("");
    create.mutate(sourceRequest(form));
  };
  const set = (patch: Partial<SourceForm>) => setForm((current) => ({ ...current, ...patch }));
  return (
    <Dialog title="Add Source" onClose={onClose}>
      <form onSubmit={submit} noValidate>
        <div className="pp-fields">
          <div className="pp-field">
            <label htmlFor={`${ids}-name`}>Name</label>
            <input
              id={`${ids}-name`}
              value={form.name}
              autoComplete="off"
              onChange={(event) => set({ name: event.target.value })}
            />
          </div>
        </div>
        <fieldset className="pp-reconcile-choice">
          <legend>Kind</legend>
          {(
            [
              ["archive", "ZIP archive"],
              ["github", "GitHub repository"],
            ] as const
          ).map(([kind, label]) => (
            <label key={kind}>
              <input
                type="radio"
                name={`${ids}-kind`}
                checked={form.kind === kind}
                onChange={() => set({ kind })}
              />
              {label}
            </label>
          ))}
        </fieldset>
        {form.kind === "github" && (
          <div className="pp-fields">
            <div className="pp-field">
              <label htmlFor={`${ids}-url`}>Repository URL</label>
              <input
                id={`${ids}-url`}
                type="url"
                value={form.url}
                autoComplete="off"
                placeholder="https://github.com/owner/repository"
                onChange={(event) => set({ url: event.target.value })}
              />
            </div>
            <div className="pp-field">
              <label htmlFor={`${ids}-branch`}>Branch</label>
              <input
                id={`${ids}-branch`}
                value={form.branch}
                autoComplete="off"
                onChange={(event) => set({ branch: event.target.value })}
              />
            </div>
          </div>
        )}
        {create.isPending && <p role="status">Adding the Source.</p>}
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
          {why && (
            <span id={`${ids}-why`} className="pp-mutation-reason">
              {why}
            </span>
          )}
        </div>
      </form>
    </Dialog>
  );
}
