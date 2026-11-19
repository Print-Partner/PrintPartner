import type { StepPartTree as PartTree } from "@print-partner/rust-client";
import { useQuery } from "@tanstack/react-query";
import { readCore } from "../core/rust";

type StepPart = PartTree["parts"][number];
type Failure = NonNullable<PartTree["failure"]>;

/** How often an open STEP file is asked about while it waits or converts. */
export const STEP_POLL_MS = 1000;

/** True for a path the Rust route opens as a STEP file. */
export function isStepPath(path: string): boolean {
  return /\.(step|stp)$/i.test(path);
}

export function stepPartTreeQuery(sourceId: number, path: string) {
  return {
    queryKey: ["core", "sources", sourceId, "step", path] as const,
    queryFn: ({ signal }: { signal: AbortSignal }) =>
      readCore("readStepPartTree", { id: sourceId }, signal, { path }),
  };
}

const FAILURES: Record<Failure, string> = {
  not_step: "This file is not a STEP file.",
  no_products: "This STEP file has no assembly structure.",
  too_large: "This assembly has too many parts or levels to show.",
  cycle: "This assembly contains itself.",
  unreadable: "The file could not be read.",
  cache_unwritable: "The part tree could not be saved.",
};

export interface PartNode {
  part: StepPart;
  children: PartNode[];
}

/** Rebuilds the nesting from the route's depth-first list: each part's children follow it one level deeper. */
export function nestParts(parts: readonly StepPart[]): PartNode[] {
  const roots: PartNode[] = [];
  const path: PartNode[] = [];
  for (const part of parts) {
    const node: PartNode = { part, children: [] };
    path.length = Math.min(path.length, part.depth);
    const parent = part.depth > 0 ? path[part.depth - 1] : undefined;
    (parent ? parent.children : roots).push(node);
    path[part.depth] = node;
  }
  return roots;
}

/**
 * The part tree of one STEP file. Rust converts the file once in the
 * background and caches the tree; this view only reads that answer, asking
 * again while the file is queued or converting.
 */
export function StepPartTree({ sourceId, path }: { sourceId: number; path: string }) {
  const query = useQuery({
    ...stepPartTreeQuery(sourceId, path),
    refetchInterval: (current) => {
      const state = current.state.data?.state;
      return state === "queued" || state === "converting" ? STEP_POLL_MS : false;
    },
  });
  if (query.isError) {
    return (
      <div role="alert" className="pp-banner" data-kind="error">
        <p>Could not open this STEP file.</p>
        <p className="pp-banner-detail">{query.error.message}</p>
        <button type="button" className="pp-button" onClick={() => void query.refetch()}>
          Retry
        </button>
      </div>
    );
  }
  const tree = query.data;
  if (!tree) return <p role="status">Opening…</p>;
  switch (tree.state) {
    case "queued":
      return <p role="status" data-state="queued">Queued for conversion.</p>;
    case "converting":
      return <p role="status" data-state="converting">Converting…</p>;
    case "failed":
      return (
        <div role="alert" className="pp-banner" data-kind="error" data-state="failed">
          <p>Conversion failed.</p>
          <p className="pp-banner-detail">
            {tree.failure ? FAILURES[tree.failure] : "The file could not be converted."}
          </p>
        </div>
      );
    case "done":
      return (
        <div data-state="done">
          <p role="status">
            {tree.parts.length === 1 ? "1 part" : `${tree.parts.length} parts`}
          </p>
          <PartList nodes={nestParts(tree.parts)} label={`Parts of ${path}`} />
        </div>
      );
  }
}

function PartList({ nodes, label }: { nodes: PartNode[]; label?: string }) {
  return (
    <ul aria-label={label}>
      {nodes.map((node, index) => (
        <li key={index}>
          {node.part.name}
          {node.part.instance && node.part.instance !== node.part.name && (
            <span className="pp-banner-detail"> ({node.part.instance})</span>
          )}
          {node.children.length > 0 && <PartList nodes={node.children} />}
        </li>
      ))}
    </ul>
  );
}
