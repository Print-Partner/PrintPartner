import { useState } from "react";
import { Link } from "react-router";
import { rowControlId } from "./ZipUpload";

/**
 * How a Source gets files: a GitHub Source syncs, and every other Source takes a ZIP upload.
 * The same rule decides which control its Library row shows.
 */
function isGithub(kind: string): boolean {
  return kind === "github";
}

/**
 * The next action under an empty Source-files view in Library. Its button presses the
 * Source's own Upload ZIP or Sync control on its row, so the row's existing flow runs and
 * this view sends nothing itself. While that row follows an import or sync, the view says so
 * and offers no button. A press that finds the row control missing or disabled says so
 * instead of doing nothing.
 */
export function LibraryFilesNext({
  sourceId,
  kind,
  running,
}: {
  sourceId: number;
  kind: string;
  running: boolean;
}) {
  const github = isGithub(kind);
  const [unavailable, setUnavailable] = useState<string | null>(null);
  if (running) {
    return (
      <p className="pp-muted">
        {github
          ? "Its sync is running. If it succeeds, its files appear here."
          : "Its import is running. If it succeeds, its files appear here."}
      </p>
    );
  }
  const control = rowControlId(sourceId, github ? "sync" : "upload");
  return (
    <p>
      {github
        ? "Sync this Source to fetch its files from GitHub. "
        : "Upload a ZIP archive of this Source to add its files. "}
      <button
        type="button"
        className="pp-button"
        onClick={() => {
          const target = document.getElementById(control);
          if (!(target instanceof HTMLInputElement || target instanceof HTMLButtonElement)) {
            setUnavailable("This Source's row is not on this page. Reload Library to find it.");
          } else if (target.disabled) {
            setUnavailable("This Source's row is busy. Its status is in the Sources table.");
          } else {
            setUnavailable(null);
            target.focus();
            target.click();
          }
        }}
      >
        {github ? "Sync" : "Upload ZIP"}
      </button>{" "}
      <span role="status">{unavailable}</span>
    </p>
  );
}

/**
 * The next action under an empty Source-files view on a Build's Sources tab: a link to the
 * Source in Library, whose row has Upload ZIP or Sync. `kind` is undefined when the Library's
 * Sources have not loaded or do not list this one.
 */
export function BuildFilesNext({ sourceId, kind }: { sourceId: number; kind: string | undefined }) {
  const to = `/library/${sourceId}`;
  if (kind === undefined) {
    return (
      <p>
        Open this Source in Library to upload a ZIP archive or sync it.{" "}
        <Link className="pp-link" to={to}>
          Open in Library
        </Link>
      </p>
    );
  }
  const github = isGithub(kind);
  return (
    <p>
      {github
        ? "Sync this Source on its Library row to fetch its files. "
        : "Upload a ZIP archive on this Source's Library row to add its files. "}
      <Link className="pp-link" to={to}>
        {github ? "Sync in Library" : "Upload ZIP in Library"}
      </Link>
    </p>
  );
}
