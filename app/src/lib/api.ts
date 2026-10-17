// Typed wrappers around the Tauri commands in src-tauri/src/commands.rs.
import { invoke } from "@tauri-apps/api/core";

export type SourceSpec =
  | { type: "local"; path: string }
  | { type: "gitRev"; repo: string; rev: string; subpath?: string }
  | { type: "gitWorktree"; repo: string };

export interface SourceOptions {
  expandArchives: boolean;
  respectGitignore: boolean;
}

export interface SourceInfo {
  kind: "folder" | "archive" | "gitRev" | "gitWorktree";
  display: string;
  rev: string | null;
  commit: string | null;
  commitSummary: string | null;
  writable: boolean;
}

export interface SessionInfo {
  id: number;
  left: SourceInfo;
  right: SourceInfo;
  isFile: boolean;
}

export type EntryKind = "file" | "dir" | "archive" | "symlink" | "submodule";
export type Status = "same" | "similar" | "different" | "leftOnly" | "rightOnly" | "error";

export interface Side {
  kind: EntryKind;
  size: number;
  mtime: number | null;
}

export interface Node {
  name: string;
  left: Side | null;
  right: Side | null;
  status: Status;
  children?: Node[];
  unexpanded?: boolean;
  message?: string;
}

export interface Summary {
  files: number;
  same: number;
  similar: number;
  different: number;
  leftOnly: number;
  rightOnly: number;
  errors: number;
  identicalSubtrees: number;
  elapsedMs: number;
}

export interface CompareResult {
  root: Node;
  summary: Summary;
}

export interface CompareOptions {
  mode: "quick" | "content";
  ignoreLineEndings: boolean;
  excludes: string[];
  compareContents: boolean;
}

export interface DiffOptions {
  whitespace: "exact" | "normalize" | "ignoreAll";
  ignoreCase: boolean;
  histogram: boolean;
}

export type RowKind = "equal" | "minor" | "changed" | "removed" | "added";

export interface LineView {
  n: number;
  t: string;
  /** Syntax colours: [start, end, 0xRRGGBB], UTF-16 offsets. */
  s: [number, number, number][];
  /** Changed character ranges. */
  m: [number, number][];
  /** Characters cut from the end of a very long line. */
  x?: number;
}

export interface RowView {
  k: RowKind;
  l: LineView | null;
  r: LineView | null;
}

export interface FileSide {
  exists: boolean;
  size: number;
  binary: boolean;
  encoding: string;
  lineEnding: "LF" | "CRLF" | "CR" | "Mixed" | "None";
  bom: boolean;
  lines: number;
}

export type View = "text" | "document" | "executable" | "firmware" | "prettyXml" | "prettyJson" | "hex" | "image";

export interface ImageSideView {
  url: string;
  width: number;
  height: number;
  format: string;
}

export interface ImageView {
  left: ImageSideView | null;
  right: ImageSideView | null;
  mask: string | null;
  differentPixels: number;
  totalPixels: number;
  bounds: [number, number, number, number] | null;
}

export interface FileDiffInfo {
  id: number;
  rows: number;
  sections: { start: number; end: number }[];
  stats: { changed: number; added: number; removed: number; minor: number };
  left: FileSide;
  right: FileSide;
  syntax: string | null;
  identical: boolean;
  view: View;
  views: { id: View; label: string }[];
  image: ImageView | null;
  note: string | null;
}

export interface RefInfo {
  name: string;
  shortId: string;
}

export interface CommitInfo {
  id: string;
  shortId: string;
  summary: string;
  time: number;
}

export interface RefList {
  head: string | null;
  branches: RefInfo[];
  remotes: RefInfo[];
  tags: RefInfo[];
  recentCommits: CommitInfo[];
}

export interface PathInfo {
  exists: boolean;
  isDir: boolean;
  isArchive: boolean;
  gitRoot: string | null;
}

export interface StartupRequest {
  left: SourceSpec;
  right: SourceSpec;
  mergeBase: boolean;
}

export const defaultCompareOptions = (): CompareOptions => ({
  mode: "content",
  ignoreLineEndings: true,
  excludes: [".git", ".svn", ".hg"],
  compareContents: true,
});

export const defaultDiffOptions = (): DiffOptions => ({ whitespace: "exact", ignoreCase: false, histogram: false });

export const api = {
  startupRequest: () => invoke<StartupRequest | null>("startup_request"),
  inspectPath: (path: string) => invoke<PathInfo>("inspect_path", { path }),
  gitRefs: (repo: string) => invoke<RefList>("git_refs", { repo }),
  openSession: (left: SourceSpec, right: SourceSpec, options: SourceOptions, mergeBase: boolean) =>
    invoke<SessionInfo>("open_session", { left, right, options, mergeBase }),
  closeSession: (id: number) => invoke<void>("close_session", { id }),
  runCompare: (id: number, options: CompareOptions) => invoke<CompareResult>("run_compare", { id, options }),
  cancelCompare: (id: number) => invoke<void>("cancel_compare", { id }),
  expand: (id: number, path: string, options: CompareOptions) => invoke<Node>("expand", { id, path, options }),
  copyEntry: (id: number, path: string, toRight: boolean, options: CompareOptions) =>
    invoke<Node>("copy_entry", { id, path, toRight, options }),
  deleteEntry: (id: number, path: string, left: boolean, options: CompareOptions) =>
    invoke<Node | null>("delete_entry", { id, path, left, options }),
  openFileDiff: (id: number, path: string, options: DiffOptions, syntax: string | null, view: View | null) =>
    invoke<FileDiffInfo>("open_file_diff", { id, path, options, syntax, view }),
  fileRows: (id: number, start: number, count: number, theme: "dark" | "light") =>
    invoke<RowView[]>("file_rows", { id, start, count, theme }),
  closeFileDiff: (id: number) => invoke<void>("close_file_diff", { id }),
  syntaxNames: () => invoke<string[]>("syntax_names"),
};

export function theme(): "dark" | "light" {
  return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

export function describeSpec(spec: SourceSpec): string {
  switch (spec.type) {
    case "local":
      return spec.path;
    case "gitRev":
      return `${spec.repo}@${spec.rev}${spec.subpath ? ":" + spec.subpath : ""}`;
    case "gitWorktree":
      return `${spec.repo} (worktree)`;
  }
}
