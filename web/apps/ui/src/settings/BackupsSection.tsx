import { routePath, routes, type RouteResponse } from "@print-partner/rust-client";
import { useId, useRef, useState } from "react";
import { CoreError, fetchCore } from "../core/client";
import { ContractError, RequestContractError, writeCore } from "../core/rust";
import { reason } from "../core/writes";
import { Dialog } from "../frame/Dialog";
import { OFFLINE_REASON, useConnection } from "../frame/connection";
import { currentRestore, restoreSent, setRestore, useRestore, type Restore } from "./restoreOutcome";

type Backup = RouteResponse<"createInstallationBackup">;
type Preview = RouteResponse<"stageInstallationRestore">;

/** The archive route. The body is a file the browser saves. */
export const ARCHIVE_PATH = "/api/v2/installation/backup/archive";

/**
 * The archive URL for one backup. The core refuses it with 409 once a later create, from any
 * window, has replaced that archive, so the file saved is always the one whose size and
 * SHA-256 this page shows.
 */
export const archiveUrl = (backup: Backup) => `${ARCHIVE_PATH}?${new URLSearchParams({ sha256: backup.sha256 })}`;

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

export const LOST_RESTORE = "The answer was lost. Reload to see which installation is open.";

const why = (failure: unknown) => reason(failure instanceof Error ? failure : new Error(String(failure)));

/** A backup's time in the viewer's locale; a value that is not a date is shown as sent. */
function timeText(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(date);
}

function sizeText(bytes: number): string {
  const mib = bytes / 1024 ** 2;
  const short = mib >= 1 ? `${mib.toFixed(1)} MiB` : `${(bytes / 1024).toFixed(1)} KiB`;
  return `${short} (${bytes.toLocaleString("en-US")} bytes)`;
}

/**
 * Posts the chosen file as the stage route's body. The browser sends its Content-Length,
 * which the core requires. The answer is checked against the generated schema.
 */
async function stageBackup(file: File): Promise<Preview> {
  const path = routePath("stageInstallationRestore", {});
  const response = await fetchCore(path, {
    method: "POST",
    headers: { accept: "application/json", "content-type": "application/gzip" },
    body: file,
  });
  const parsed = routes.stageInstallationRestore.response.safeParse(
    await response.json().catch(() => undefined),
  );
  if (!parsed.success) throw new ContractError(path);
  return parsed.data;
}

/**
 * Only an answer that proves nothing changed is a refusal: a request never sent, a 4xx (the
 * route and its gate decide every 4xx before the restore is queued), `restore_rolled_back`, and
 * `restore_busy`, the one 503 the core sends before activation changes anything. An unknown or
 * rolled-back stage needs the file again. Any other answer may follow a swap, so it is lost: a
 * lost answer, an unreadable 200, any other 503 (the core sends a code-less 503 when the
 * writer's reply is lost after the restore was queued), `restart_required`, `outcome_unknown`
 * or any 5xx.
 */
function restoreFailure(failure: unknown): Restore {
  if (failure instanceof CoreError && failure.code === "restore_not_found") {
    return {
      kind: "refused",
      text: "The core no longer holds this check, so this installation is unchanged. Choose the file again and check it.",
      chooseAgain: true,
    };
  }
  if (failure instanceof CoreError && failure.code === "restore_rolled_back") {
    // The core removes the stage when it rolls back, so the same id cannot be sent again.
    return {
      kind: "refused",
      text: `The restore was refused: ${why(failure)}. This installation is unchanged. Choose the file again to retry.`,
      chooseAgain: true,
    };
  }
  const unchanged =
    failure instanceof RequestContractError ||
    (failure instanceof CoreError &&
      (failure.status < 500 || (failure.status === 503 && failure.code === "restore_busy")));
  if (unchanged) {
    return { kind: "refused", text: `The restore was refused: ${why(failure)}. This installation is unchanged.` };
  }
  const said = failure instanceof CoreError && failure.detail ? `${failure.detail}. ` : "";
  return { kind: "lost", text: `${said}${LOST_RESTORE}` };
}

function CreateBackup({ offline, locked }: { offline: boolean; locked: boolean }) {
  const [creating, setCreating] = useState(false);
  const [backup, setBackup] = useState<Backup | null>(null);
  const [note, setNote] = useState("");
  const sending = useRef(false);
  const fetching = useRef(false);
  const reasonId = useId();
  const off = offline || creating || locked;
  const create = async () => {
    if (off || sending.current) return;
    sending.current = true;
    setCreating(true);
    setNote("");
    try {
      setBackup(await writeCore("createInstallationBackup", {}));
    } catch (failure) {
      if (failure instanceof CoreError || failure instanceof RequestContractError) {
        // A refused create leaves the core's kept backup as it was.
        setNote(`The backup was not made: ${why(failure)}.`);
      } else {
        // The core may have made a new backup and dropped the one shown here.
        setBackup(null);
        setNote("The core did not confirm the backup. Create backup again to download one.");
      }
    } finally {
      sending.current = false;
      setCreating(false);
    }
  };
  /**
   * Asks the core with HEAD whether it still keeps this archive, then starts the download. A
   * replaced or dropped archive removes the backup shown here, so a different file is never
   * saved under these facts. The GET carries the digest too, so a create between the two
   * requests is refused rather than sent.
   */
  const download = async (shown: Backup) => {
    if (fetching.current) return;
    fetching.current = true;
    setNote("");
    try {
      await fetchCore(archiveUrl(shown), { method: "HEAD", headers: { accept: "*/*" } });
      save(archiveUrl(shown), shown.name);
    } catch (failure) {
      if (failure instanceof CoreError && (failure.status === 404 || failure.status === 409)) {
        setBackup((now) => (now === shown ? null : now));
        setNote(
          "The core no longer keeps the backup shown here; a newer backup may have been made in another window. Create backup again to download one.",
        );
      } else {
        setNote(`The download did not start: ${why(failure)}. Try Download again.`);
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
      {creating && <p role="status">Creating backup</p>}
      {note && <p role="alert" className="pp-field-error">{note}</p>}
      {backup && (
        <dl className="pp-facts" aria-label="Backup made">
          <div className="pp-fact">
            <dt>File</dt>
            <dd>
              <span className="num pp-digest">{backup.name}</span>
              <span className="pp-fact-detail">
                <a
                  href={archiveUrl(backup)}
                  download={backup.name}
                  onClick={(event) => {
                    event.preventDefault();
                    void download(backup);
                  }}
                >
                  Download
                </a>
              </span>
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
          {backup.warnings.includes("large") && (
            <p className="pp-fact-detail">This backup is large. Check there is room where you save it.</p>
          )}
        </dl>
      )}
      <p>This page keeps only the backup made since it opened. After a reload, create one again.</p>
    </>
  );
}

/**
 * Asks before replacing the installation. Focus starts on Cancel; only "Replace this
 * installation" calls the restore route. While the restore runs, the dialog cannot close.
 */
function RestoreDialog({
  preview,
  restore,
  offline,
  onReplace,
  onClose,
}: {
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
      {restore === null && (
        <>
          <p>This installation's Builds, Sources and history are replaced by the backup's.</p>
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
              onClick={onReplace}
            >
              Replace this installation
            </button>
          </div>
        </>
      )}
      {running && <p role="status">Restoring. Keep this page open.</p>}
      {restore?.kind === "done" && <p role="status">Restore complete</p>}
      {(restore?.kind === "refused" || restore?.kind === "lost") && (
        <p role="alert" className="pp-field-error">{restore.text}</p>
      )}
      {restore && !running && (
        <div className="pp-dialog-actions">
          {restore.kind === "refused" && (
            <button type="button" className="pp-button" onClick={onClose}>
              Close
            </button>
          )}
          {restore.kind !== "refused" && <ReloadButton />}
        </div>
      )}
    </Dialog>
  );
}

/** Loads the page from the server, so no earlier read is kept. */
function ReloadButton() {
  return (
    <button type="button" className="pp-button pp-button-primary" onClick={() => window.location.reload()}>
      Reload
    </button>
  );
}

/** The restore's outcome in the section, after its dialog is closed or Settings reopened. */
function RestoreNote({ restore }: { restore: Restore }) {
  if (restore.kind === "restoring") return <p role="status">Restoring. Keep this page open.</p>;
  return (
    <>
      <p role={restore.kind === "done" ? "status" : "alert"}>
        {restore.kind === "done" ? "Restore complete" : restore.text}
      </p>
      {restore.kind !== "refused" && <ReloadButton />}
    </>
  );
}

/** What the core read from the checked file, and the button that opens the dialog. */
function CheckedBackup({ preview, offline, reasonId, canRestore, onRestore }: {
  preview: Preview;
  offline: boolean;
  reasonId: string;
  canRestore: boolean;
  onRestore: () => void;
}) {
  return (
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
      {canRestore && (
        <button
          type="button"
          className="pp-button pp-button-primary"
          aria-disabled={offline || undefined}
          aria-describedby={offline ? reasonId : undefined}
          onClick={() => !offline && onRestore()}
        >
          Restore this backup
        </button>
      )}
    </>
  );
}

type Check =
  | { kind: "none" }
  | { kind: "checking" }
  | { kind: "checked"; preview: Preview }
  | { kind: "refused"; text: string };

function RestoreBackup({ offline, restore }: { offline: boolean; restore: Restore | null }) {
  const fileId = useId();
  const reasonId = useId();
  const [file, setFile] = useState<File | null>(null);
  // Bumped to empty the file field when the core no longer holds its check.
  const [field, setField] = useState(0);
  const [check, setCheck] = useState<Check>({ kind: "none" });
  const [confirming, setConfirming] = useState(false);
  const checking = useRef(false);
  const settled = restoreSent(restore);
  const checkOff = offline || !file || check.kind === "checking" || settled;
  const preview = check.kind === "checked" ? check.preview : null;

  const checkBackup = async () => {
    if (checkOff || !file || checking.current) return;
    checking.current = true;
    setCheck({ kind: "checking" });
    setRestore(null);
    try {
      setCheck({ kind: "checked", preview: await stageBackup(file) });
    } catch (failure) {
      const text =
        failure instanceof CoreError
          ? `The backup was not accepted: ${why(failure)}. This installation is unchanged.`
          : "The core did not answer the check. This installation is unchanged; check the backup again.";
      setCheck({ kind: "refused", text });
    } finally {
      checking.current = false;
    }
  };

  const replace = async () => {
    // Read from the store, not this render: a second click before React renders sees the send.
    if (!preview || offline || restoreSent(currentRestore())) return;
    setRestore({ kind: "restoring" });
    let outcome: Restore;
    try {
      await writeCore("restoreInstallation", {}, {
        restore_id: preview.restore_id,
        confirmation: preview.confirmation,
      });
      outcome = { kind: "done" };
    } catch (failure) {
      outcome = restoreFailure(failure);
    }
    // Settings may have closed while the request ran; the store keeps the outcome either way.
    setRestore(outcome);
    if (outcome.kind !== "refused") return;
    if (outcome.chooseAgain) {
      setConfirming(false);
      setCheck({ kind: "none" });
      setFile(null);
      setField((n) => n + 1);
    }
  };

  return (
    <>
      <h3>Restore from a backup</h3>
      <p>A restore replaces this whole installation and cannot be undone here.</p>
      <p>To bring back an earlier Plan of one Build, use that Build's History instead.</p>
      <p>If Print Partner restarts before you confirm, choose the file again.</p>
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
            if (restore?.kind === "refused") setRestore(null);
          }}
        />
      </div>
      {offline && <p id={reasonId}>{OFFLINE_REASON}</p>}
      <button
        type="button"
        className="pp-button"
        aria-disabled={checkOff || undefined}
        aria-describedby={offline ? reasonId : undefined}
        onClick={() => void checkBackup()}
      >
        Check backup
      </button>
      {check.kind === "checking" && <p role="status">Checking backup</p>}
      {check.kind === "refused" && <p role="alert" className="pp-field-error">{check.text}</p>}
      {preview && (
        <CheckedBackup
          preview={preview}
          offline={offline}
          reasonId={reasonId}
          canRestore={!settled}
          onRestore={() => {
            setRestore(null);
            setConfirming(true);
          }}
        />
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
 * Settings → Backups: make one whole-installation backup and download it, and replace this
 * installation with a backup file after a confirmation. Nothing is sent when the section opens
 * or a file is chosen; the restore route is called only from the dialog.
 */
export function BackupsSection() {
  const offline = useConnection() !== "online";
  const restore = useRestore();
  return (
    <>
      <CreateBackup offline={offline} locked={restoreSent(restore)} />
      <RestoreBackup offline={offline} restore={restore} />
    </>
  );
}
