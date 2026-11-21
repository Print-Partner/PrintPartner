import { routePath, routes, type RouteResponse } from "@print-partner/rust-client";
import { useId, useRef, useState } from "react";
import { CoreError, fetchCore } from "../core/client";
import { ContractError, RequestContractError } from "../core/rust";
import { reason } from "../core/writes";
import { Dialog } from "../frame/Dialog";
import { OFFLINE_REASON, useConnection } from "../frame/connection";
import { restoreAnswer, restoreVerdict } from "./restoreAnswer";
import { restoreSent, setRestore, useRestore, type Restore } from "./restoreOutcome";

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

/**
 * Sends one restore. A redirect is refused rather than followed, so a 307 or 308 from a layer in
 * front of the core cannot send the restore a second time; the refused redirect is a network
 * error, which `restoreVerdict` reads as an unknown outcome.
 */
async function sendRestore(preview: Preview): Promise<void> {
  const path = routePath("restoreInstallation", {});
  const request = routes.restoreInstallation.request.safeParse({
    restore_id: preview.restore_id,
    confirmation: preview.confirmation,
  });
  if (!request.success) throw new RequestContractError(path);
  const response = await fetchCore(path, {
    method: "POST",
    headers: { accept: "application/json", "content-type": "application/json" },
    body: JSON.stringify(request.data),
    redirect: "error",
  });
  const parsed = routes.restoreInstallation.response.safeParse(await response.json().catch(() => undefined));
  if (!parsed.success) throw new ContractError(path);
}

export const LOST_RESTORE = "The answer was lost. Reload to see which installation is open.";

/** What the section shows for a failed restore; `restoreVerdict` alone decides which. */
function restoreFailure(failure: unknown): Restore {
  const verdict = restoreVerdict(restoreAnswer(failure));
  if (verdict === "unknown") return { kind: "lost", text: LOST_RESTORE };
  if (verdict === "unchanged") {
    return { kind: "refused", text: `The restore was refused: ${why(failure)}. This installation is unchanged.` };
  }
  // The core no longer holds the stage: unknown id, or removed by the rollback.
  const text =
    failure instanceof CoreError && failure.code === "restore_not_found"
      ? "The core no longer holds this check, so this installation is unchanged. Choose the file again and check it."
      : `The restore was refused: ${why(failure)}. This installation is unchanged. Choose the file again to retry.`;
  return { kind: "refused", text, chooseAgain: true };
}

function sizeText(bytes: number): string {
  const mib = bytes / 1024 ** 2;
  const short = mib >= 1 ? `${mib.toFixed(1)} MiB` : `${(bytes / 1024).toFixed(1)} KiB`;
  return `${short} (${bytes.toLocaleString("en-US")} bytes)`;
}

/**
 * Settings → Backups: make one whole-installation backup, show what the core made and download
 * it, check a backup file, and replace this installation with it after a confirmation. Nothing is
 * sent when the section opens or a file is chosen; only the dialog calls the restore route.
 */
export function BackupsSection() {
  const offline = useConnection() !== "online";
  const restore = useRestore();
  const locked = restoreSent(restore);
  const [creating, setCreating] = useState(false);
  const [backup, setBackup] = useState<Backup | null>(null);
  const [note, setNote] = useState("");
  // Mounted from the start, so a screen reader announces each change while focus stays put.
  const [status, setStatus] = useState("");
  const sending = useRef(false);
  // The ref refuses a second press in the same tick; the state turns the button off while HEAD runs.
  const fetching = useRef(false);
  const [downloading, setDownloading] = useState(false);
  const reasonId = useId();
  const off = offline || creating || locked;
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
    setDownloading(true);
    setStatus("Checking the core still keeps this backup");
    setNote("");
    try {
      const href = archiveUrl(shown);
      const answer = await fetchCore(href, { method: "HEAD", headers: { accept: "*/*" }, redirect: "error" });
      // Only a 200 says the core will send this archive; any other 2xx saves nothing.
      if (answer.status !== 200) throw new CoreError(answer.status, href);
      save(href, shown.name);
      setStatus("Download started");
    } catch (failure) {
      setStatus("");
      if (failure instanceof CoreError && (failure.status === 404 || failure.status === 409)) {
        // A HEAD answer has no body, so the status alone says the archive is gone or replaced.
        setBackup((now) => (now === shown ? null : now));
        setNote("The core no longer keeps this backup; a newer one may have been made in another window. Create backup again.");
      } else if (failure instanceof CoreError && failure.status === 503) {
        // A create or another download holds the core's one backup slot.
        setNote("The core is busy with another backup or download. Try Download again in a moment.");
      } else {
        // A redirect is refused as a network error and never followed. Nothing was saved, and the
        // GET is bound to the shown digest, so trying again cannot save a different file.
        const said = failure instanceof CoreError ? `the core answered ${failure.status}` : "the core did not answer";
        setNote(`The download did not start: ${said}. Try Download again.`);
      }
    } finally {
      fetching.current = false;
      setDownloading(false);
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
                aria-disabled={offline || downloading || undefined}
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
      <CheckBackup offline={offline} reasonId={reasonId} restore={restore} />
    </>
  );
}

type Check =
  | { kind: "none" }
  | { kind: "checking" }
  | { kind: "checked"; preview: Preview }
  | { kind: "refused"; text: string };

/**
 * "Restore from a backup": posts the chosen file to the stage route, shows what the core read
 * from it, and offers Restore this backup. The restore's state lives in `restoreOutcome`, so a
 * sent restore keeps this part locked after Settings closes and opens again.
 */
function CheckBackup({ offline, reasonId, restore }: { offline: boolean; reasonId: string; restore: Restore | null }) {
  const fileId = useId();
  const [file, setFile] = useState<File | null>(null);
  // Bumped to empty the file field when the core no longer holds its check.
  const [field, setField] = useState(0);
  const [check, setCheck] = useState<Check>({ kind: "none" });
  const [confirming, setConfirming] = useState(false);
  // Mounted from the start, so a screen reader announces each change while focus stays put.
  const [status, setStatus] = useState("");
  const checking = useRef(false);
  const settled = restoreSent(restore);
  const off = offline || !file || check.kind === "checking" || settled;
  const preview = check.kind === "checked" ? check.preview : null;
  const checkBackup = async () => {
    if (off || !file || checking.current) return;
    checking.current = true;
    setCheck({ kind: "checking" });
    setRestore(null);
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
  const replace = async () => {
    // Reads the store, not this render, so a second press before React renders sends nothing.
    if (!preview || offline || restoreSent()) return;
    setRestore({ kind: "restoring" });
    let outcome: Restore;
    try {
      await sendRestore(preview);
      outcome = { kind: "done" };
    } catch (failure) {
      outcome = restoreFailure(failure);
    }
    // Settings may have closed while the request ran; the store keeps the outcome either way.
    setRestore(outcome);
    if (outcome.kind === "refused" && outcome.chooseAgain) {
      setConfirming(false);
      setCheck({ kind: "none" });
      setFile(null);
      setField((n) => n + 1);
    }
  };
  return (
    <>
      <h3>Restore from a backup</h3>
      <p>
        Choose a backup file and check it. Checking reads the file and changes nothing here. A
        restore then replaces this whole installation and cannot be undone here; to bring back an
        earlier Plan of one Build, use that Build's History instead.
      </p>
      <div className="pp-field">
        <label htmlFor={fileId}>Backup file</label>
        <input
          key={field}
          id={fileId}
          type="file"
          accept=".gz,.tgz,application/gzip"
          disabled={settled || check.kind === "checking"}
          onChange={(event) => {
            setFile(event.target.files?.[0] ?? null);
            setCheck({ kind: "none" });
            setStatus("");
            if (restore?.kind === "refused") setRestore(null);
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
      {preview && (
        <>
          <dl className="pp-facts" aria-label="Backup checked">
            <div className="pp-fact">
              <dt>Made</dt>
              <dd>{timeText(preview.created_at)}</dd>
            </div>
            <div className="pp-fact">
              <dt>App version</dt>
              <dd>{preview.app_version}</dd>
            </div>
            <div className="pp-fact">
              <dt>Schema version</dt>
              <dd className="num">{preview.schema_version}</dd>
            </div>
          </dl>
          {!settled && (
            <button
              type="button"
              className="pp-button pp-button-primary"
              aria-disabled={offline || undefined}
              aria-describedby={offline ? reasonId : undefined}
              onClick={() => {
                if (offline) return;
                setRestore(null);
                setConfirming(true);
              }}
            >
              Restore this backup
            </button>
          )}
        </>
      )}
      {restore && !confirming && <RestoreNote restore={restore} />}
      {confirming && preview && (
        <RestoreDialog
          preview={preview}
          restore={restore}
          offline={offline}
          onReplace={() => void replace()}
          onClose={() => setConfirming(false)}
        />
      )}
    </>
  );
}

/**
 * Asks before replacing the installation. Focus starts on Cancel; only "Replace this
 * installation" calls the restore route. While the restore runs, the dialog cannot close.
 */
function RestoreDialog({ preview, restore, offline, onReplace, onClose }: {
  preview: Preview;
  restore: Restore | null;
  offline: boolean;
  onReplace: () => void;
  onClose: () => void;
}) {
  const reasonId = useId();
  const running = restore?.kind === "restoring";
  return (
    <Dialog title="Replace this installation?" onClose={() => !running && onClose()}>
      {restore === null ? (
        <>
          <p>This installation's Builds, Sources, Plans and history are replaced by the backup's. The core asks you to confirm with:</p>
          <blockquote data-confirmation>{preview.confirmation}</blockquote>
          {offline && <p id={reasonId}>{OFFLINE_REASON}</p>}
          <div className="pp-dialog-actions">
            <button type="button" className="pp-button" onClick={onClose}>
              Cancel
            </button>
            <button
              type="button"
              className="pp-button pp-button-primary"
              aria-disabled={offline || undefined}
              aria-describedby={offline ? reasonId : undefined}
              onClick={() => !offline && onReplace()}
            >
              Replace this installation
            </button>
          </div>
        </>
      ) : (
        <>
          <RestoreNote restore={restore} />
          {restore.kind === "refused" && (
            <div className="pp-dialog-actions">
              <button type="button" className="pp-button" onClick={onClose}>
                Close
              </button>
            </div>
          )}
        </>
      )}
    </Dialog>
  );
}

/** The restore's state: running, complete, refused, or lost with Reload as the only action. */
function RestoreNote({ restore }: { restore: Restore }) {
  if (restore.kind === "restoring") return <p role="status">Restoring. Keep this page open.</p>;
  if (restore.kind === "refused") return <p role="alert" className="pp-field-error">{restore.text}</p>;
  return (
    <>
      {restore.kind === "done" ? (
        <p role="status">Restore complete. Reload to open the restored installation; it may ask you to sign in.</p>
      ) : (
        <p role="alert" className="pp-field-error">{restore.text}</p>
      )}
      <div className="pp-dialog-actions">
        <button type="button" className="pp-button pp-button-primary" onClick={() => window.location.reload()}>
          Reload
        </button>
      </div>
    </>
  );
}
