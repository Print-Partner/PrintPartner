import { routePath, routes } from "@print-partner/rust-client";
import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { Link } from "react-router";
import { CoreError, fetchCore } from "../core/client";
import { ContractError, readCore, writeCore } from "../core/rust";
import { syncFailure, syncStartFailure } from "./GithubSync";
import type { StartLibraryJob } from "./libraryJobs";
import { revisionText, syncText, type Line, type Source } from "./sources";

// LibraryScreen exports LineCell for Build Sources. A second copy here keeps
// this row from importing that screen.
function LineCell({ line }: { line: Line }) {
  return (
    <>
      {line.label}
      {line.detail && <span className="pp-fact-detail">{line.detail}</span>}
    </>
  );
}

/** How often a pending or running import or sync is read. No other status is polled. */
export const IMPORT_POLL_MS = 2000;

/** The DOM id of a Library row's Upload ZIP input or Sync button. One Library renders at a time. */
export function rowControlId(sourceId: number, control: "upload" | "sync"): string {
  return `pp-source-${sourceId}-${control}`;
}

/** The strings in `result.warnings`, when that list is present. */
export function importWarnings(result: unknown): string[] {
  if (typeof result !== "object" || result === null || !("warnings" in result)) return [];
  const { warnings } = result;
  if (!Array.isArray(warnings)) return [];
  return warnings.filter((item): item is string => typeof item === "string");
}

/**
 * Posts one ZIP. The answer is checked against `uploadSourceZip`. A refusal or a lost
 * answer throws, and this function does not try again.
 */
export async function uploadSourceZip(sourceId: number, file: File): Promise<{ job_id: string }> {
  const path = routePath("uploadSourceZip", { id: sourceId });
  const form = new FormData();
  form.append("file", file, file.name);
  const response = await fetchCore(path, {
    method: "POST",
    headers: { accept: "application/json" },
    body: form,
  });
  const parsed = routes.uploadSourceZip.response.safeParse(
    await response.json().catch(() => undefined),
  );
  if (!parsed.success) throw new ContractError(path);
  return parsed.data;
}

function uploadFailure(error: unknown): string {
  if (error instanceof CoreError) return error.detail ?? error.message;
  const message = error instanceof Error ? error.message : "The upload was not confirmed";
  return `${message} It was not sent again.`;
}

type Phase =
  | { kind: "idle" }
  | { kind: "uploading" }
  | { kind: "importing"; progress: number | null }
  | { kind: "failed"; message: string }
  | { kind: "refused"; message: string }
  | { kind: "done"; warnings: string[] };

function syncLine(source: Source, phase: Phase): Line {
  const github = source.source_kind === "github";
  switch (phase.kind) {
    case "uploading":
      return { label: "Uploading", detail: null };
    case "importing":
      return {
        label: github ? "Syncing" : "Importing",
        detail: phase.progress == null ? null : `${phase.progress}%`,
      };
    case "failed":
      return {
        label: github ? "Sync failed" : "Import failed",
        detail: phase.message || null,
      };
    case "refused":
      return { label: phase.message, detail: null };
    default:
      return syncText(source);
  }
}

function isAbort(error: unknown): boolean {
  return error instanceof Error && error.name === "AbortError";
}

/**
 * One Library row, plus a warnings row when a finished import listed any.
 * `activity` is the Latest activity cell. A remembered job is followed here;
 * choosing a file is the only thing that uploads.
 */
export function SourceRows({
  source,
  activity,
  jobId,
  note,
  starting,
  start,
  forget,
  selected = false,
}: {
  source: Source;
  activity: ReactNode;
  jobId: string | undefined;
  note: string | undefined;
  starting: boolean;
  start: StartLibraryJob;
  forget: (sourceId: number, jobId: string) => void;
  /** This row's files are the ones on screen. Clicking the name clears that. */
  selected?: boolean;
}) {
  const client = useQueryClient();
  const [jobPhase, setPhase] = useState<Phase>(() =>
    jobId ? { kind: "importing", progress: null } : { kind: "idle" },
  );
  const github = source.source_kind === "github";
  let phase = jobPhase;
  if (starting && !jobId) {
    phase = github ? { kind: "importing", progress: null } : { kind: "uploading" };
  } else if (!jobId && note && jobPhase.kind === "idle") {
    phase = { kind: "refused", message: note };
  }
  // Busy while this tab's map holds a job for the Source, the fact the files panel reads,
  // or while this tab's own send is unanswered. A job learned from another tab counts at
  // once, before its first status read.
  const busy = jobId != null || starting;

  // A job that left the map while this row still showed it running was ended by another
  // tab, which read its result. Stop showing it and read the Source again.
  const followed = useRef(jobId);
  useEffect(() => {
    const previous = followed.current;
    followed.current = jobId;
    if (previous && !jobId) {
      setPhase((current) => (current.kind === "importing" ? { kind: "idle" } : current));
      void client.invalidateQueries({ queryKey: ["core", "sources"] });
    } else if (jobId && jobId !== previous) {
      // A job started or replaced by another tab: show it as running.
      setPhase({ kind: "importing", progress: null });
    }
  }, [jobId, client]);

  useEffect(() => {
    if (!jobId) return;
    const controller = new AbortController();
    let timer = 0;
    let stopped = false;
    const stop = () => {
      stopped = true;
      window.clearTimeout(timer);
      controller.abort();
    };
    // A failed read is not a new status: the job is still the one being followed,
    // including while the core is down. 404 means the core has forgotten it.
    const schedule = () => {
      if (!stopped) timer = window.setTimeout(read, IMPORT_POLL_MS);
    };
    const read = () => {
      void readCore("readJob", { id: jobId }, controller.signal).then(
        (job) => {
          if (stopped) return;
          if (job.status === "pending" || job.status === "running") {
            setPhase({ kind: "importing", progress: job.progress });
            schedule();
            return;
          }
          if (job.status === "error") {
            setPhase({ kind: "failed", message: job.error ?? "" });
          } else if (job.status === "done") {
            const named =
              source.source_kind === "github" ? syncFailure(source.id, job.result) : null;
            setPhase(
              named
                ? { kind: "failed", message: named }
                : { kind: "done", warnings: importWarnings(job.result) },
            );
            void client.invalidateQueries({ queryKey: ["core", "sources"] });
          } else {
            setPhase({ kind: "idle" });
          }
          forget(source.id, jobId);
        },
        (error: unknown) => {
          if (stopped || isAbort(error)) return;
          if (error instanceof CoreError && error.status === 404) {
            setPhase({ kind: "idle" });
            forget(source.id, jobId);
            void client.invalidateQueries({ queryKey: ["core", "sources"] });
            return;
          }
          schedule();
        },
      );
    };
    read();
    return stop;
  }, [jobId, source.id, source.source_kind, forget, client]);

  const send = (file: File) => {
    if (busy) return;
    if (!file.name.toLowerCase().endsWith(".zip")) {
      setPhase({ kind: "refused", message: "Choose a .zip file." });
      return;
    }
    setPhase({ kind: "idle" });
    void start(source.id, () => uploadSourceZip(source.id, file), uploadFailure);
  };

  const sync = () => {
    if (busy) return;
    setPhase({ kind: "idle" });
    void start(
      source.id,
      () => writeCore("startSourceSync", {}, { project_ids: [source.id] }),
      syncStartFailure,
    );
  };

  const live = phase.kind === "idle" || phase.kind === "done" ? undefined : "status";
  return (
    <>
      <tr data-selected={selected || undefined}>
        <th scope="row">
          <Link
            className="pp-link pp-source-name"
            to={selected ? "/library" : `/library/${source.id}`}
            aria-current={selected ? "true" : undefined}
          >
            {source.name}
          </Link>
          {!source.content_available && <span className="pp-tag">No files on disk</span>}
          <span className="pp-fact-detail">
            {[source.source_type, source.category].filter(Boolean).join(", ")}
          </span>
        </th>
        <td>
          <LineCell line={revisionText(source)} />
        </td>
        <td>
          <div className={github ? "pp-sync-cell" : undefined}>
            <span role={live}>
              <LineCell line={syncLine(source, phase)} />
            </span>
            {github && (
              <button
                type="button"
                id={rowControlId(source.id, "sync")}
                className="pp-button"
                aria-label={`Sync ${source.name}`}
                disabled={busy}
                onClick={sync}
              >
                Sync
              </button>
            )}
          </div>
        </td>
        <td>{activity}</td>
        <td>
          {source.source_kind !== "github" && (
            <label className="pp-button pp-file-button">
              Upload ZIP
              <input
                id={rowControlId(source.id, "upload")}
                className="pp-visually-hidden"
                type="file"
                accept=".zip,application/zip"
                aria-label={`Upload ZIP for ${source.name}`}
                disabled={busy}
                onChange={(event) => {
                  const file = event.target.files?.[0];
                  event.target.value = "";
                  if (file) send(file);
                }}
              />
            </label>
          )}
        </td>
      </tr>
      {phase.kind === "done" && phase.warnings.length > 0 && (
        <tr>
          <td colSpan={5}>
            <ul className="pp-reasons" aria-label={`Warnings for ${source.name}`}>
              {phase.warnings.map((warning, index) => (
                <li key={`${index}-${warning}`}>{warning}</li>
              ))}
            </ul>
          </td>
        </tr>
      )}
    </>
  );
}
