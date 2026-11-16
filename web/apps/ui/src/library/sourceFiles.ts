import { routePath, type SourceFileList } from "@print-partner/rust-client";
import { readCore } from "../core/rust";

export type SourceFile = SourceFileList["files"][number];

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

/** A `/library/:sourceId` segment, or null when it does not name a Source. */
export function selectedSourceId(value: string | undefined): number | null {
  if (!value || !/^[1-9]\d*$/.test(value)) return null;
  const id = Number(value);
  return Number.isSafeInteger(id) ? id : null;
}

export function fileName(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash === -1 ? path : path.slice(slash + 1);
}

/** Parent folder, or "" for a file at the revision root. */
export function folderOf(path: string): string {
  const slash = path.lastIndexOf("/");
  return slash === -1 ? "" : path.slice(0, slash);
}

export interface FileGroup<T extends { path: string }> {
  folder: string;
  files: T[];
}

/** Groups in the order their folders first appear. `files` stays in path order inside each group. */
export function groupByFolder<T extends { path: string }>(files: readonly T[]): FileGroup<T>[] {
  const groups: FileGroup<T>[] = [];
  const byFolder = new Map<string, FileGroup<T>>();
  for (const file of files) {
    const folder = folderOf(file.path);
    let group = byFolder.get(folder);
    if (!group) {
      group = { folder, files: [] };
      byFolder.set(folder, group);
      groups.push(group);
    }
    group.files.push(file);
  }
  return groups;
}

/** Whole bytes under 1024, then KB, then MB, in steps of 1024. */
export function fileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const scaled = (value: number, name: string) => {
    const shown = value >= 10 ? Math.round(value) : Math.round(value * 10) / 10;
    return `${shown} ${name}`;
  };
  const kb = bytes / 1024;
  if (kb < 1024) return scaled(kb, "KB");
  return scaled(kb / 1024, "MB");
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
