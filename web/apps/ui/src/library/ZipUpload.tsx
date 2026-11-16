import { routePath, routes } from "@print-partner/rust-client";
import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { CoreError, fetchCore } from "../core/client";
import { ContractError, readCore, writeCore } from "../core/rust";
import { syncFailure, syncStartFailure } from "./GithubSync";
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

/** Source id to the import or sync job this browser started. A reload reads it back. */
export const JOBS_KEY = "pp.library.jobs";

/** How often a pending or running import or sync is read. No other status is polled. */
export const IMPORT_POLL_MS = 2000;

type JobMap = Record<string, string>;

export function readJobs(): JobMap {
  try {
    const raw = localStorage.getItem(JOBS_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return {};
    const jobs: JobMap = {};
    for (const [id, job] of Object.entries(parsed)) {
      if (typeof job === "string" && job !== "") jobs[id] = job;
    }
    return jobs;
  } catch {
    return {};
  }
}

function writeJobs(jobs: JobMap) {
  try {
    localStorage.setItem(JOBS_KEY, JSON.stringify(jobs));
  } catch {
    // A blocked store still follows the job until this page closes.
  }
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

export function useLibraryJobs() {
  const [jobs, setJobs] = useState(readJobs);
  const [refusals, setRefusals] = useState<Record<string, string>>({});
  const remember = useCallback((sourceId: number, jobId: string) => {
    const next = { ...readJobs(), [String(sourceId)]: jobId };
    writeJobs(next);
    setJobs(next);
    setRefusals((current) => {
      if (!(String(sourceId) in current)) return current;
      const rest = { ...current };
      delete rest[String(sourceId)];
      return rest;
    });
  }, []);
  const forget = useCallback((sourceId: number) => {
    const next = readJobs();
    if (!(String(sourceId) in next)) return;
    delete next[String(sourceId)];
    writeJobs(next);
    setJobs({ ...next });
  }, []);
  const refuse = useCallback((sourceId: number, message: string) => {
    setRefusals((current) => ({ ...current, [String(sourceId)]: message }));
  }, []);
  return { jobs, refusals, remember, forget, refuse };
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
  remember,
  forget,
}: {
  source: Source;
  activity: ReactNode;
  jobId: string | undefined;
  note: string | undefined;
  remember: (sourceId: number, jobId: string) => void;
  forget: (sourceId: number) => void;
}) {
  const client = useQueryClient();
  const [phase, setPhase] = useState<Phase>(() =>
    jobId
      ? { kind: "importing", progress: null }
      : note
        ? { kind: "refused", message: note }
        : { kind: "idle" },
  );
  const sending = useRef(false);
  const busy = phase.kind === "uploading" || phase.kind === "importing";

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
          forget(source.id);
        },
        (error: unknown) => {
          if (stopped || isAbort(error)) return;
          if (error instanceof CoreError && error.status === 404) {
            setPhase({ kind: "idle" });
            forget(source.id);
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
    if (sending.current || busy) return;
    if (!file.name.toLowerCase().endsWith(".zip")) {
      setPhase({ kind: "refused", message: "Choose a .zip file." });
      return;
    }
    sending.current = true;
    setPhase({ kind: "uploading" });
    void uploadSourceZip(source.id, file).then(
      (accepted) => {
        // Stored before any further await, so a reload follows this job and does not upload again.
        remember(source.id, accepted.job_id);
        setPhase({ kind: "importing", progress: null });
        sending.current = false;
      },
      (error: unknown) => {
        setPhase({ kind: "refused", message: uploadFailure(error) });
        sending.current = false;
      },
    );
  };

  const sync = () => {
    if (sending.current || busy) return;
    sending.current = true;
    setPhase({ kind: "importing", progress: null });
    void writeCore("startSourceSync", {}, { project_ids: [source.id] }).then(
      (accepted) => {
        // Stored before any further await, so a reload follows this job and does not sync again.
        remember(source.id, accepted.job_id);
        sending.current = false;
      },
      (error: unknown) => {
        setPhase({ kind: "refused", message: syncStartFailure(error) });
        sending.current = false;
      },
    );
  };

  const live = phase.kind === "idle" || phase.kind === "done" ? undefined : "status";
  const github = source.source_kind === "github";
  return (
    <>
      <tr>
        <th scope="row">
          {source.name}
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
