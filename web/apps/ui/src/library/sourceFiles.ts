import type { SourceFileList } from "@print-partner/rust-client";

export type SourceFile = SourceFileList["files"][number];

export { sourceFilesQuery, thumbnailUrl } from "../screen/sourceThumb";

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
