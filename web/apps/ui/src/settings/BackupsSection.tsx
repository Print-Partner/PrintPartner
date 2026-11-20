import { routePath, routes, type RouteResponse } from "@print-partner/rust-client";
import { useId, useRef, useState } from "react";
import { CoreError, fetchCore } from "../core/client";
import { ContractError } from "../core/rust";
import { reason } from "../core/writes";
import { OFFLINE_REASON, useConnection } from "../frame/connection";

type Backup = RouteResponse<"createInstallationBackup">;
type Preview = RouteResponse<"stageInstallationRestore">;

/**
 * The archive URL for one backup. The core answers 409 once a later create, from any window, has
 * replaced that archive, so the file saved is always the one whose size and SHA-256 are shown.
 */
const archiveUrl = (backup: Backup) =>
  `/api/v2/installation/backup/archive?${new URLSearchParams({ sha256: backup.sha256 })}`;

/** Starts a browser download of `href`. The session cookie authenticates the same-origin GET. */
function save(href: string, name: string): void {
  const anchor = document.createElement("a");
  anchor.href = href;
  anchor.download = name;
  anchor.hidden = true;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
}

const why = (failure: unknown) => reason(failure instanceof Error ? failure : new Error(String(failure)));

/** A backup's time in the viewer's locale; a value that is not a date is shown as sent. */
function timeText(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date);
}

/**
 * Posts one create. A redirect is refused, not followed: a followed 307 or 308 would send the
 * POST a second time, and the destination's answer is not the core's. The refused redirect is a
 * network error, which the section reads as not confirmed.
 */
async function createBackup(): Promise<Backup> {
  const path = routePath("createInstallationBackup", {});
  const response = await fetchCore(path, {
    method: "POST",
    headers: { accept: "application/json" },
    redirect: "error",
  });
  const parsed = routes.createInstallationBackup.response.safeParse(await response.json().catch(() => undefined));
  if (!parsed.success) throw new ContractError(path);
  return parsed.data;
}

/**
 * Posts the chosen file as the stage route's body. The browser sends its Content-Length, which
 * the core requires. A redirect is refused, so the answer read is always the core's.
 */
async function stageBackup(file: File): Promise<Preview> {
  const path = routePath("stageInstallationRestore", {});
  const response = await fetchCore(path, {
    method: "POST",
    headers: { accept: "application/json", "content-type": "application/gzip" },
    body: file,
    redirect: "error",
  });
  const parsed = routes.stageInstallationRestore.response.safeParse(await response.json().catch(() => undefined));
  if (!parsed.success) throw new ContractError(path);
  return parsed.data;
}

function sizeText(bytes: number): string {
  const mib = bytes / 1024 ** 2;
  const short = mib >= 1 ? `${mib.toFixed(1)} MiB` : `${(bytes / 1024).toFixed(1)} KiB`;
  return `${short} (${bytes.toLocaleString("en-US")} bytes)`;
}

/**
 * Settings → Backups: make one whole-installation backup, show what the core made and download
 * it, and check a backup file. Nothing is sent when the section opens or a file is chosen. This
 * section never calls the restore route.
 */
export function BackupsSection() {
  const offline = useConnection() !== "online";
  const [creating, setCreating] = useState(false);
  const [backup, setBackup] = useState<Backup | null>(null);
  const [note, setNote] = useState("");
  // Mounted from the start, so a screen reader announces each change while focus stays put.
  const [status, setStatus] = useState("");
  const sending = useRef(false);
  const fetching = useRef(false);
  const reasonId = useId();
  const off = offline || creating;
  const create = async () => {
    if (off || sending.current) return;
    sending.current = true;
    setCreating(true);
    setStatus("Creating backup");
    setNote("");
    try {
      setBackup(await createBackup());
      setStatus("Backup made");
    } catch (failure) {
      setStatus("");
      if (failure instanceof CoreError && failure.problem) {
        // The core keeps a new archive only when it answers 200, so its own refusal leaves the
        // kept backup as it was.
        setNote(`The backup was not made: ${why(failure)}.`);
      } else {
        // A redirect, a proxy's page, a malformed problem, a cut-off body or a lost answer: the
        // core may have made a new backup and dropped the one shown here. Creating again is
        // safe; it replaces it.
        setBackup(null);
        setNote("The core did not confirm the backup. Create backup again.");
      }
    } finally {
      sending.current = false;
      setCreating(false);
    }
  };
  /**
   * Asks with HEAD whether the core still keeps the shown archive, then starts the browser
   * download of the same URL. The GET carries the digest too, so a create between the two is
   * refused rather than saved under these facts.
   */
  const download = async (shown: Backup) => {
    if (offline || fetching.current) return;
    fetching.current = true;
    setStatus("Checking the core still keeps this backup");
    setNote("");
    try {
      await fetchCore(archiveUrl(shown), { method: "HEAD", headers: { accept: "*/*" }, redirect: "error" });
      save(archiveUrl(shown), shown.name);
      setStatus("Download started");
    } catch (failure) {
      setStatus("");
      if (failure instanceof CoreError && (failure.status === 404 || failure.status === 409)) {
        // A HEAD answer has no body, so the status alone says the archive is gone or replaced.
        setBackup((now) => (now === shown ? null : now));
        setNote("The core no longer keeps this backup; a newer one may have been made in another window. Create backup again.");
      } else {
        // A redirect is refused as a network error and never followed. Nothing was saved, and the
        // GET is bound to the shown digest, so trying again cannot save a different file.
        const said = failure instanceof CoreError ? `the core answered ${failure.status}` : "the core did not answer";
        setNote(`The download did not start: ${said}. Try Download again.`);
      }
    } finally {
      fetching.current = false;
    }
  };
  return (
    <>
      <p>
        A backup holds the database and the files of this whole installation: Builds, Sources,
        Plans and history.
      </p>
      {offline && <p id={reasonId}>{OFFLINE_REASON}</p>}
      <button
        type="button"
        className="pp-button pp-button-primary"
        aria-disabled={off || undefined}
        aria-describedby={offline ? reasonId : undefined}
        onClick={() => void create()}
      >
        Create backup
      </button>
      <p role="status">{status}</p>
      {note && <p role="alert" className="pp-field-error">{note}</p>}
      {backup && (
        <dl className="pp-facts" aria-label="Backup made">
          <div className="pp-fact">
            <dt>File</dt>
            <dd>
              <span className="num pp-digest">{backup.name}</span>{" "}
              <button
                type="button"
                className="pp-button"
                aria-disabled={offline || undefined}
                aria-describedby={offline ? reasonId : undefined}
                onClick={() => void download(backup)}
              >
                Download
              </button>
            </dd>
          </div>
          <div className="pp-fact">
            <dt>Made</dt>
            <dd>{timeText(backup.created_at)}, app version {backup.app_version}</dd>
          </div>
          <div className="pp-fact">
            <dt>Size</dt>
            <dd className="num">{sizeText(backup.size_bytes)}</dd>
          </div>
          <div className="pp-fact">
            <dt>SHA-256</dt>
            <dd className="num pp-digest">{backup.sha256}</dd>
          </div>
        </dl>
      )}
      {backup?.warnings.includes("large") && (
        <p className="pp-fact-detail">This backup is large. Check there is room where you keep it.</p>
      )}
      <p>This page keeps only the backup made since it opened. After a reload, create one again.</p>
      <CheckBackup offline={offline} reasonId={reasonId} />
    </>
  );
}

type Check =
  | { kind: "none" }
  | { kind: "checking" }
  | { kind: "checked"; preview: Preview }
  | { kind: "refused"; text: string };

/**
 * "Restore from a backup", first part: posts the chosen file to the stage route and shows what
 * the core read from it. Staging never changes this installation. There is no Restore button.
 */
function CheckBackup({ offline, reasonId }: { offline: boolean; reasonId: string }) {
  const fileId = useId();
  const [file, setFile] = useState<File | null>(null);
  const [check, setCheck] = useState<Check>({ kind: "none" });
  // Mounted from the start, so a screen reader announces each change while focus stays put.
  const [status, setStatus] = useState("");
  const checking = useRef(false);
  const off = offline || !file || check.kind === "checking";
  const checkBackup = async () => {
    if (off || !file || checking.current) return;
    checking.current = true;
    setCheck({ kind: "checking" });
    setStatus("Checking backup");
    try {
      setCheck({ kind: "checked", preview: await stageBackup(file) });
      setStatus("Backup checked");
    } catch (failure) {
      setStatus("");
      const text =
        failure instanceof CoreError && failure.problem
          ? `The backup was not accepted: ${why(failure)}. This installation is unchanged.`
          : "The core did not confirm the check. This installation is unchanged. Check backup again.";
      setCheck({ kind: "refused", text });
    } finally {
      checking.current = false;
    }
  };
  return (
    <>
      <h3>Restore from a backup</h3>
      <p>Choose a backup file and check it. Checking reads the file and changes nothing here.</p>
      <div className="pp-field">
        <label htmlFor={fileId}>Backup file</label>
        <input
          id={fileId}
          type="file"
          accept=".gz,.tgz,application/gzip"
          disabled={check.kind === "checking"}
          onChange={(event) => {
            setFile(event.target.files?.[0] ?? null);
            setCheck({ kind: "none" });
            setStatus("");
          }}
        />
      </div>
      <button
        type="button"
        className="pp-button"
        aria-disabled={off || undefined}
        aria-describedby={offline ? reasonId : undefined}
        onClick={() => void checkBackup()}
      >
        Check backup
      </button>
      <p role="status">{status}</p>
      {check.kind === "refused" && <p role="alert" className="pp-field-error">{check.text}</p>}
      {check.kind === "checked" && (
        <dl className="pp-facts" aria-label="Backup checked">
          <div className="pp-fact">
            <dt>Made</dt>
            <dd>{timeText(check.preview.created_at)}</dd>
          </div>
          <div className="pp-fact">
            <dt>App version</dt>
            <dd>{check.preview.app_version}</dd>
          </div>
          <div className="pp-fact">
            <dt>Schema version</dt>
            <dd className="num">{check.preview.schema_version}</dd>
          </div>
        </dl>
      )}
    </>
  );
}
