import { useQuery } from "@tanstack/react-query";
import { useId, useState } from "react";
import { CoreError, fetchCore, type UnavailableSourceFile } from "../core/client";
import { reason } from "../core/writes";
import { buildQuery } from "./BuildWorkspace";

type Download = "stl-pack" | "checklist";

/** The two accepted Plan exports (GRE-1447, GRE-1525). They answer files, not JSON. */
const exportPath = (build: number, download: Download, remaining = false) =>
  `/api/v2/plans/${build}/exports/${download}${download === "stl-pack" && remaining ? "?scope=remaining" : ""}`;

/**
 * Downloads one export in two steps, so the page never holds the file:
 * 1. Request it with `fetchCore`. A refusal throws a `CoreError` (a 401 also opens sign-in)
 *    and nothing is saved. On success, abort without reading the body.
 * 2. Start a browser download of the same path. The session cookie authenticates a
 *    same-origin GET, and the browser saves the file under the server's filename.
 * The core builds the file twice for one download; the ticket accepts that cost.
 */
async function download(path: string): Promise<void> {
  const controller = new AbortController();
  await fetchCore(path, { headers: { accept: "*/*" }, signal: controller.signal });
  controller.abort();
  const anchor = document.createElement("a");
  anchor.href = path;
  anchor.download = "";
  anchor.hidden = true;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
}

const REASONS: Readonly<Record<string, string>> = {
  missing: "is missing",
  digest_mismatch: "changed since the Plan was accepted",
  empty: "is empty",
  symlink: "is a link, not a file",
  not_file: "is not a file",
  ambiguous_case: "matches more than one file when case is ignored",
  unsafe_path: "has a path outside its Source",
  io_error: "could not be read",
};

/** A Source file the pack refused, in words. An unknown reason reads "is unavailable". */
const fileText = (file: UnavailableSourceFile) =>
  `${file.source}: ${file.relative_path} ${REASONS[file.reason] ?? "is unavailable"}`;

type Refusal = { download: Download; detail: string; files: readonly UnavailableSourceFile[] };

function Refused({ refusal }: { refusal: Refusal }) {
  return (
    <div role="alert" className="pp-banner" data-kind="error">
      <p>{refusal.detail}</p>
      {refusal.files.length > 0 && (
        <ul>
          {refusal.files.map((file) => (
            <li key={`${file.source}/${file.relative_path}`}>{fileText(file)}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

/**
 * "Download STL pack" (with "Remaining units only") and "Download checklist", at the top of
 * the Production tab. Each is busy until the core answers the first request; after a
 * refusal it can be used again. A Build with no accepted Plan gets one line instead.
 */
export function ExportActions({ build }: { build: number }) {
  const summary = useQuery(buildQuery(build));
  const [remaining, setRemaining] = useState(false);
  const [busy, setBusy] = useState<Download | null>(null);
  const [refusal, setRefusal] = useState<Refusal | null>(null);
  const ids = { remaining: useId(), help: useId() };

  // An accepted Plan is an accepted input set. Freshness can be `untracked` with
  // `no_accepted_inputs` for a set stored in the older format (`format_version` 1), which the
  // exports still serve, so only an explicit null set means "no accepted Plan". Freshness
  // `unavailable` says nothing about the Plan: the actions stay, and the core refuses with
  // 409 `no_accepted_plan` if there is none.
  const freshness = summary.data?.build.freshness;
  if (
    freshness &&
    "accepted_input_set_id" in freshness &&
    freshness.accepted_input_set_id === null
  ) {
    return (
      <p className="pp-muted">No Plan has been accepted, so there is nothing to download yet.</p>
    );
  }

  const start = async (which: Download) => {
    if (busy) return;
    setBusy(which);
    setRefusal(null);
    try {
      await download(exportPath(build, which, remaining));
    } catch (error) {
      // A 401 opens sign-in through fetchCore's listener; there is nothing to add here.
      if (!(error instanceof CoreError && error.status === 401)) {
        setRefusal({
          download: which,
          detail: error instanceof Error ? reason(error) : String(error),
          files: error instanceof CoreError ? error.files : [],
        });
      }
    } finally {
      setBusy(null);
    }
  };

  const button = (which: Download, label: string, describedBy?: string) => (
    <button
      type="button"
      className="pp-button"
      aria-disabled={busy !== null || undefined}
      aria-describedby={describedBy}
      onClick={() => void start(which)}
    >
      {busy === which ? "Preparing download" : label}
    </button>
  );

  return (
    <section className="pp-export-actions" aria-label="Downloads">
      <div className="pp-export-action">
        {button("stl-pack", "Download STL pack", ids.help)}
        <label htmlFor={ids.remaining}>
          <input
            id={ids.remaining}
            type="checkbox"
            checked={remaining}
            onChange={(event) => setRemaining(event.target.checked)}
          />{" "}
          Remaining units only
        </label>
        <p id={ids.help} className="pp-muted">
          If the download fails part-way, a Source file changed while it was being packed. Download
          again.
        </p>
        {refusal?.download === "stl-pack" && <Refused refusal={refusal} />}
      </div>
      <div className="pp-export-action">
        {button("checklist", "Download checklist")}
        {refusal?.download === "checklist" && <Refused refusal={refusal} />}
      </div>
    </section>
  );
}
