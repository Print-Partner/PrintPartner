import { routePath } from "@print-partner/rust-client";
import { readCore } from "../core/rust";

export function sourceFilesQuery(sourceId: number) {
  return {
    queryKey: ["core", "sources", sourceId, "files"] as const,
    queryFn: ({ signal }: { signal: AbortSignal }) =>
      readCore("listSourceFiles", { id: sourceId }, signal),
  };
}

/** PNG for one STL, beside `listSourceFiles` on the same Source. */
export function thumbnailUrl(sourceId: number, path: string): string {
  const files = routePath("listSourceFiles", { id: sourceId });
  return `${files.slice(0, -"files".length)}thumbnail?path=${encodeURIComponent(path)}`;
}

/** True when any pixel of `element` is inside the viewport. */
export function isInView(element: Element): boolean {
  const box = element.getBoundingClientRect();
  const view = element.ownerDocument.defaultView;
  const height = view?.innerHeight ?? 0;
  const width = view?.innerWidth ?? 0;
  return (
    box.width > 0 &&
    box.height > 0 &&
    box.bottom > 0 &&
    box.right > 0 &&
    box.top < height &&
    box.left < width
  );
}

/** The first STL in a `listSourceFiles` list. That route sorts by path. */
export function firstStlPath(files: readonly { path: string; kind: string }[]): string | null {
  return files.find((file) => file.kind === "stl")?.path ?? null;
}
