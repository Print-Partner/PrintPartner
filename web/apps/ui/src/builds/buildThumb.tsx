import { useQuery } from "@tanstack/react-query";
import { useLayoutEffect, useRef, useState, type RefObject } from "react";
import { readCore } from "../core/rust";
import { StlThumb, ThumbFallback } from "../screen/StlThumb";
import { firstStlPath, isInView, sourceFilesQuery } from "../screen/sourceThumb";

/** The Source on the lowest `layer_order` that has one. */
export function firstSourceId(
  layers: readonly { layer_order: number; project_id: number | null }[],
): number | null {
  let chosen: { order: number; id: number } | null = null;
  for (const layer of layers) {
    if (layer.project_id == null) continue;
    if (chosen == null || layer.layer_order < chosen.order) {
      chosen = { order: layer.layer_order, id: layer.project_id };
    }
  }
  return chosen?.id ?? null;
}

/**
 * Build ids whose row has been on screen. Rows below the fold are not read
 * until they scroll in, so the list does not fetch a file list per Build.
 */
export function useVisibleBuildIds(
  bodyRef: RefObject<HTMLTableSectionElement | null>,
  rowKey: string,
): ReadonlySet<number> {
  const [visible, setVisible] = useState<ReadonlySet<number>>(() => new Set());
  const known = useRef(new Set<number>());
  useLayoutEffect(() => {
    const body = bodyRef.current;
    if (!body) return;
    const found: number[] = [];
    const pending: HTMLElement[] = [];
    for (const el of body.querySelectorAll<HTMLElement>("[data-build]")) {
      const id = Number(el.dataset.build);
      if (!Number.isInteger(id) || known.current.has(id)) continue;
      if (isInView(el)) found.push(id);
      else pending.push(el);
    }
    if (found.length > 0) {
      for (const id of found) known.current.add(id);
      setVisible(new Set(known.current));
    }
    if (pending.length === 0 || typeof IntersectionObserver === "undefined") return;
    const observer = new IntersectionObserver((entries) => {
      let changed = false;
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const id = Number((entry.target as HTMLElement).dataset.build);
        if (!Number.isInteger(id) || known.current.has(id)) continue;
        known.current.add(id);
        changed = true;
        observer.unobserve(entry.target);
      }
      if (changed) setVisible(new Set(known.current));
    });
    for (const el of pending) observer.observe(el);
    return () => observer.disconnect();
  }, [bodyRef, rowKey]);
  return visible;
}

function FailedThumb() {
  return (
    <span className="pp-stl-thumb">
      <ThumbFallback failed />
    </span>
  );
}

function ActiveThumb({ buildId }: { buildId: number }) {
  const layers = useQuery({
    queryKey: ["core", "build", buildId, "layers"] as const,
    queryFn: ({ signal }: { signal: AbortSignal }) =>
      readCore("listBuildLayers", { id: buildId }, signal),
  });
  const sourceId = layers.data ? firstSourceId(layers.data.layers) : null;
  const files = useQuery({
    ...sourceFilesQuery(sourceId ?? 0),
    enabled: sourceId != null,
  });
  const waiting =
    (layers.data == null && !layers.isError) ||
    (sourceId != null && files.data == null && !files.isError);
  if (waiting) return <span className="pp-stl-thumb" />;
  if (
    layers.isError ||
    files.isError ||
    sourceId == null ||
    files.data == null ||
    files.data.revision_id == null
  ) {
    return <FailedThumb />;
  }
  const path = firstStlPath(files.data.files);
  if (path == null) return <FailedThumb />;
  return <StlThumb sourceId={sourceId} path={path} />;
}

/** The Build's first Source thumbnail, or an empty box until the row is on screen. */
export function BuildSourceThumb({ buildId, active }: { buildId: number; active: boolean }) {
  if (!active) return <span className="pp-stl-thumb" data-build={buildId} />;
  return <ActiveThumb buildId={buildId} />;
}
