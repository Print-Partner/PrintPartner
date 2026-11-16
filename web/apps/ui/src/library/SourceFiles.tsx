import type { SourceFileList } from "@print-partner/rust-client";
import { useQuery } from "@tanstack/react-query";
import { useCallback, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  fileName,
  fileSize,
  groupByFolder,
  isInView,
  sourceFilesQuery,
  thumbnailUrl,
  type SourceFile,
} from "./sourceFiles";

type ThumbStatus = "off" | "pending" | "done";

/**
 * Files of the selected Source, grouped by folder. An STL thumbnail is requested
 * once its row is in the viewport; a failed thumbnail is a placeholder.
 */
export function SourceFiles({ sourceId, sourceName }: { sourceId: number; sourceName: string }) {
  const query = useQuery(sourceFilesQuery(sourceId));
  const headingId = useId();
  const [thumbsBusy, setThumbsBusy] = useState(false);
  const onBusy = useCallback((busy: boolean) => {
    setThumbsBusy((current) => (current === busy ? current : busy));
  }, []);
  return (
    <section
      className="pp-source-files"
      aria-labelledby={headingId}
      aria-busy={query.isPending || thumbsBusy || undefined}
    >
      <h2 id={headingId}>Files for {sourceName}</h2>
      {query.isError && (
        <div role="alert" className="pp-banner" data-kind="error">
          <p>Could not load this Source&apos;s files.</p>
          <p className="pp-banner-detail">{query.error.message}</p>
          <button type="button" className="pp-button" onClick={() => void query.refetch()}>
            Retry
          </button>
        </div>
      )}
      {query.data && <FileBody list={query.data} sourceId={sourceId} onBusy={onBusy} />}
    </section>
  );
}

function FileBody({
  list,
  sourceId,
  onBusy,
}: {
  list: SourceFileList;
  sourceId: number;
  onBusy: (busy: boolean) => void;
}) {
  const hasThumbs =
    list.revision_id != null && list.files.some((file) => file.kind === "stl");
  useLayoutEffect(() => {
    if (!hasThumbs) onBusy(false);
  }, [hasThumbs, onBusy]);
  if (list.revision_id == null) return <p>No files yet.</p>;
  if (list.files.length === 0) return <p>This revision has no files.</p>;
  return <GroupedFiles files={list.files} sourceId={sourceId} onBusy={onBusy} />;
}

function GroupedFiles({
  files,
  sourceId,
  onBusy,
}: {
  files: SourceFile[];
  sourceId: number;
  onBusy: (busy: boolean) => void;
}) {
  const stlKey = files
    .filter((file) => file.kind === "stl")
    .map((file) => file.path)
    .join("\0");
  const stlPaths = useMemo(() => (stlKey === "" ? [] : stlKey.split("\0")), [stlKey]);
  const statuses = useRef(new Map<string, ThumbStatus>());
  const report = useCallback(
    (path: string, next: ThumbStatus) => {
      const known = statuses.current;
      if (known.get(path) === next) return;
      known.set(path, next);
      const busy = stlPaths.some((item) => {
        const status = known.get(item);
        return status !== "off" && status !== "done";
      });
      onBusy(busy);
    },
    [stlPaths, onBusy],
  );
  useLayoutEffect(() => {
    if (stlPaths.length === 0) onBusy(false);
  }, [stlPaths, onBusy]);
  return (
    <>
      {groupByFolder(files).map((group) => (
        <section key={group.folder || "."} className="pp-file-group">
          <h3>{group.folder || "Top level"}</h3>
          <ul className="pp-file-list">
            {group.files.map((file) => (
              <li key={file.path} className="pp-file-row">
                {file.kind === "stl" && (
                  <StlThumb sourceId={sourceId} path={file.path} report={report} />
                )}
                <span className="pp-file-name">{fileName(file.path)}</span>
                <span className="pp-file-size">{fileSize(file.size_bytes)}</span>
              </li>
            ))}
          </ul>
        </section>
      ))}
    </>
  );
}

function StlThumb({
  sourceId,
  path,
  report,
}: {
  sourceId: number;
  path: string;
  report: (path: string, status: ThumbStatus) => void;
}) {
  const ref = useRef<HTMLSpanElement>(null);
  const [show, setShow] = useState(false);
  const [failed, setFailed] = useState(false);
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    if (isInView(element)) {
      setShow(true);
      report(path, "pending");
      return;
    }
    report(path, "off");
    if (typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver((entries) => {
      if (!entries.some((entry) => entry.isIntersecting)) return;
      setShow(true);
      report(path, "pending");
      observer.disconnect();
    });
    observer.observe(element);
    return () => observer.disconnect();
  }, [path, report]);
  return (
    <span className="pp-stl-thumb" ref={ref} data-path={path}>
      {show && !failed ? (
        <img
          src={thumbnailUrl(sourceId, path)}
          alt=""
          width={64}
          height={64}
          onLoad={() => report(path, "done")}
          onError={() => {
            setFailed(true);
            report(path, "done");
          }}
        />
      ) : (
        <span
          className="pp-stl-placeholder"
          role={failed ? "img" : undefined}
          aria-label={failed ? "No thumbnail" : undefined}
        >
          {failed ? "No preview" : null}
        </span>
      )}
    </span>
  );
}
