import { routePath, routes, type RouteResponse } from "@print-partner/rust-client";
import { useId, useRef, useState } from "react";
import { CoreError, fetchCore } from "../core/client";
import { ContractError } from "../core/rust";
import { reason } from "../core/writes";
import { OFFLINE_REASON, useConnection } from "../frame/connection";

type Backup = RouteResponse<"createInstallationBackup">;

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

function sizeText(bytes: number): string {
  const mib = bytes / 1024 ** 2;
  const short = mib >= 1 ? `${mib.toFixed(1)} MiB` : `${(bytes / 1024).toFixed(1)} KiB`;
  return `${short} (${bytes.toLocaleString("en-US")} bytes)`;
}

/**
 * Settings → Backups: make one whole-installation backup and show what the core made. Nothing is
 * sent when the section opens.
 */
export function BackupsSection() {
  const offline = useConnection() !== "online";
  const [creating, setCreating] = useState(false);
  const [backup, setBackup] = useState<Backup | null>(null);
  const [note, setNote] = useState("");
  // Mounted from the start, so a screen reader announces each change while focus stays put.
  const [status, setStatus] = useState("");
  const sending = useRef(false);
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
            <dd className="num pp-digest">{backup.name}</dd>
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
    </>
  );
}
