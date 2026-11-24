import type { CreateSourceRequest, GithubBranchList } from "@print-partner/rust-client";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useId, useRef, useState, type FormEvent } from "react";
import { CoreError } from "../core/client";
import { readCore, writeCore } from "../core/rust";
import { OFFLINE_REASON, useConnection } from "../frame/connection";
import { Dialog } from "../frame/Dialog";
import { syncStartFailure } from "./GithubSync";
import { sourcesQuery } from "../core/sources";
import type { StartLibraryJob } from "./libraryJobs";

type SourceKind = "github" | "archive";

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

type BranchField = { kind: "text"; reason: string } | { kind: "list"; names: string[] };

function branchReason(error: unknown): string {
  if (error instanceof CoreError) return error.detail ?? error.message;
  return error instanceof Error ? error.message : "Could not read the branches.";
}

/**
 * Library's "Add Source": a name and a kind, ZIP archive or GitHub repository. Create sends
 * `createSource` once and never again by itself; the core refuses a name already in use, so
 * pressing Create after a lost answer cannot add a second Source. A GitHub Source then starts
 * one sync. Leaving the URL field reads that repository's branches.
 */
export function AddSource({ start }: { start: StartLibraryJob }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" className="pp-button pp-button-primary" onClick={() => setOpen(true)}>
        Add Source
      </button>
      {open && <AddSourceDialog onClose={() => setOpen(false)} start={start} />}
    </>
  );
}

function AddSourceDialog({ onClose, start }: { onClose: () => void; start: StartLibraryJob }) {
  const client = useQueryClient();
  const offline = useConnection() === "offline";
  const ids = useId();
  const [form, setForm] = useState<SourceForm>(EMPTY);
  const [failure, setFailure] = useState("");
  const [branches, setBranches] = useState<BranchField>({ kind: "text", reason: "" });
  const branchRead = useRef(0);
  const sending = useRef(false);
  const create = useMutation({
    retry: false,
    networkMode: "always",
    mutationFn: async (body: CreateSourceRequest) => {
      const source = await writeCore("createSource", {}, body);
      if (body.source_kind === "github") {
        await start(
          source.id,
          () => writeCore("startSourceSync", {}, { project_ids: [source.id] }),
          syncStartFailure,
        );
      }
    },
    // Runs even after the dialog closed: the list is read again whatever the answer.
    onSettled: () => {
      sending.current = false;
      void client.invalidateQueries({ queryKey: sourcesQuery.queryKey });
    },
  });
  const why = offline ? OFFLINE_REASON : create.isPending ? "Adding the Source." : missing(form);
  const blocked = why !== null || create.isPending;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (blocked || sending.current) return;
    sending.current = true;
    setFailure("");
    // Per-call callbacks do not run once the dialog has closed, so a late answer
    // never closes or writes into a dialog opened after it.
    create.mutate(sourceRequest(form), {
      onSuccess: onClose,
      onError: (error: Error) => {
        // A 4xx is the core's refusal; anything else may have added the Source.
        const refused = error instanceof CoreError && error.status < 500;
        setFailure(
          refused
            ? (error.detail ?? error.message)
            : `Could not confirm the Source was added: ${error.message}. The list is read again; check it before you press Create again.`,
        );
      },
    });
  };
  const set = (patch: Partial<SourceForm>) => {
    // A refusal describes the values that were sent; editing makes it stale.
    setFailure("");
    setForm((current) => ({ ...current, ...patch }));
  };
  const loadBranches = (raw: string) => {
    const url = raw.trim();
    const token = ++branchRead.current;
    if (!url) {
      setBranches({ kind: "text", reason: "" });
      return;
    }
    void readCore("listGithubBranches", {}, undefined, { url }).then(
      (listing: GithubBranchList) => {
        if (token !== branchRead.current) return;
        const names =
          !listing.default_branch || listing.branches.includes(listing.default_branch)
            ? listing.branches
            : [listing.default_branch, ...listing.branches];
        const chosen =
          (listing.default_branch && names.includes(listing.default_branch)
            ? listing.default_branch
            : names[0]) ?? "";
        if (!chosen) {
          setBranches({ kind: "text", reason: "The repository listed no branches." });
          return;
        }
        setBranches({ kind: "list", names });
        set({ branch: chosen });
      },
      (error: unknown) => {
        if (token !== branchRead.current) return;
        setBranches({ kind: "text", reason: branchReason(error) });
      },
    );
  };
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
                onChange={(event) => {
                  branchRead.current += 1;
                  setBranches({ kind: "text", reason: "" });
                  set({ url: event.target.value });
                }}
                onBlur={(event) => loadBranches(event.target.value)}
              />
            </div>
            <div className="pp-field">
              <label htmlFor={`${ids}-branch`}>Branch</label>
              {branches.kind === "list" ? (
                <select
                  id={`${ids}-branch`}
                  value={form.branch}
                  onChange={(event) => set({ branch: event.target.value })}
                >
                  {branches.names.map((name) => (
                    <option key={name} value={name}>
                      {name}
                    </option>
                  ))}
                </select>
              ) : (
                <input
                  id={`${ids}-branch`}
                  value={form.branch}
                  autoComplete="off"
                  aria-invalid={branches.reason ? true : undefined}
                  aria-describedby={branches.reason ? `${ids}-branch-reason` : undefined}
                  onChange={(event) => set({ branch: event.target.value })}
                />
              )}
              {branches.kind === "text" && branches.reason && (
                <p id={`${ids}-branch-reason`} role="alert" className="pp-field-error">
                  {branches.reason}
                </p>
              )}
            </div>
          </div>
        )}
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
