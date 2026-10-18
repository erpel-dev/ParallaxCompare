// Typed wrappers around the Tauri commands in src-tauri/src/commands.rs.
import { invoke } from "@tauri-apps/api/core";

export type SourceSpec =
  | { type: "local"; path: string }
  | { type: "gitRev"; repo: string; rev: string; subpath?: string }
  | { type: "gitWorktree"; repo: string };

/** A source as text, the form the CLI and the path bars read: `repo@rev:sub` for Git. */
export function sourceText(spec: SourceSpec): string {
  switch (spec.type) {
    case "local":
      return spec.path;
    case "gitRev":
      return `${spec.repo}@${spec.rev}${spec.subpath ? `:${spec.subpath}` : ""}`;
    case "gitWorktree":
      return spec.repo;
  }
}

export interface FileHistory {
  /** The file in the working tree, when the repository has one. */
  worktree: SourceSpec | null;
  /** The commits that changed the file, newest first. */
  commits: (CommitInfo & { spec: SourceSpec })[];
}

/** The last part of a source: a file or folder name, or a Git revision. */
export function sourceName(spec: SourceSpec): string {
  if (spec.type === "gitRev" && !spec.subpath) return spec.rev;
  return sourceText(spec).replace(/[\\/]+$/, "").split(/[\\/:!]/).pop() ?? "";
}

export interface SourceOptions {
  expandArchives: boolean;
  respectGitignore: boolean;
}

export interface SourceInfo {
  kind: "folder" | "file" | "archive" | "gitRev" | "gitWorktree";
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
  /** The file's last line, without the line break the other file has. */
  nn?: boolean;
}

export interface SearchOptions {
  matchCase: boolean;
  wholeWord: boolean;
  regex: boolean;
}

/** A match: its row, side (0 left, 1 right) and UTF-16 offsets in the line. */
export interface Hit {
  row: number;
  side: 0 | 1;
  start: number;
  end: number;
}

export interface SearchResult {
  hits: Hit[];
  truncated: boolean;
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
  editable: boolean;
}

export type View =
  | "text"
  | "document"
  | "executable"
  | "firmware"
  | "prettyXml"
  | "prettyJson"
  | "hex"
  | "image"
  | "model"
  | "chatTemplate"
  | "chatPreview"
  | "metadata"
  | "tokenizer"
  | "tensors"
  | "classOutline"
  | "bytecode"
  | "bytecodeLines"
  | "databaseSchema"
  | "databaseRows"
  | "dataSummary"
  | "modelGraph"
  | "wasmOutline"
  | "wasmText"
  | "assemblyOutline"
  | "il"
  | "androidXml"
  | "canDatabase"
  | "a2lObjects"
  | "memoryMap"
  | "debugSymbols"
  | "measurement"
  | "calibration"
  | "imageMetadata"
  | "mediaSummary"
  | "dependencies"
  | "notebook"
  | "notebookCode"
  | "normalizedYaml"
  | "normalizedToml"
  | "reflowedText"
  | "needs"
  | "templateOutline"
  | "conanGraph"
  | "normalizedConan"
  | "pythonOutline"
  | "perlOutline"
  | "rustOutline"
  | "jsOutline"
  | "xpandOutline"
  | "xtendOutline"
  | "ecucValues"
  | "table"
  | "certificate"
  | "email";

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
  area: string | null;
  outline: string | null;
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

export type SideName = "left" | "right";

/** A position as JavaScript counts: line and UTF-16 column. */
export interface Pos {
  line: number;
  col: number;
}

/** A column past the end of the line: the end of the line. */
export const END = 0xffffffff;

export interface EditInfo {
  rows: number;
  sections: { start: number; end: number }[];
  stats: { changed: number; added: number; removed: number; minor: number };
  lines: [number, number];
  dirty: [boolean, boolean];
  exists: [boolean, boolean];
  canUndo: boolean;
  canRedo: boolean;
  identical: boolean;
  focusRow: number | null;
  caret: { row: number; line: number; col: number } | null;
}

export interface OpResult {
  nodes: (Node | null)[];
  error: string | null;
  notTrashed: string[];
}

export interface PathsChanged {
  session: number;
  paths: string[];
  full: boolean;
  /** The file comparison that saved, when the change came from a save. */
  origin?: number;
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
  parseSource: (text: string) => invoke<SourceSpec>("parse_source", { text }),
  sideSource: (id: number, left: boolean, rel: string) => invoke<SourceSpec>("side_source", { id, left, rel }),
  completePath: (text: string) => invoke<string[]>("complete_path", { text }),
  sourceParent: (spec: SourceSpec) => invoke<SourceSpec | null>("source_parent", { spec }),
  openSource: (spec: SourceSpec, reveal: boolean) => invoke<void>("open_source", { spec, reveal }),
  fileHistory: (spec: SourceSpec) => invoke<FileHistory>("file_history", { spec }),
  openSession: (left: SourceSpec, right: SourceSpec, options: SourceOptions, mergeBase: boolean) =>
    invoke<SessionInfo>("open_session", { left, right, options, mergeBase }),
  closeSession: (id: number) => invoke<void>("close_session", { id }),
  /** `invalidate`: a refresh, which reads cached archives and folder snapshots again. */
  runCompare: (id: number, options: CompareOptions, invalidate = false) =>
    invoke<CompareResult>("run_compare", { id, options, invalidate }),
  cancelCompare: (id: number) => invoke<void>("cancel_compare", { id }),
  expand: (id: number, path: string, options: CompareOptions) => invoke<Node>("expand", { id, path, options }),
  copyEntries: (id: number, paths: string[], toRight: boolean, options: CompareOptions) =>
    invoke<OpResult>("copy_entries", { id, paths, toRight, options }),
  deleteEntries: (id: number, paths: string[], left: boolean, permanent: boolean, options: CompareOptions) =>
    invoke<OpResult>("delete_entries", { id, paths, left, permanent, options }),
  cancelOp: (id: number) => invoke<void>("cancel_op", { id }),
  refreshPaths: (id: number, paths: string[], options: CompareOptions) =>
    invoke<(Node | null)[]>("refresh_paths", { id, paths, options }),
  openFileDiff: (id: number, path: string, options: DiffOptions, syntax: string | null, view: View | null) =>
    invoke<FileDiffInfo>("open_file_diff", { id, path, options, syntax, view }),
  fileRows: (id: number, start: number, count: number, theme: "dark" | "light") =>
    invoke<RowView[]>("file_rows", { id, start, count, theme }),
  searchDiff: (id: number, query: string, options: SearchOptions) =>
    invoke<SearchResult>("search_diff", { id, query, options }),
  closeFileDiff: (id: number) => invoke<void>("close_file_diff", { id }),
  editText: (id: number, side: SideName, from: Pos, to: Pos, text: string, caret: number | null) =>
    invoke<EditInfo>("edit_text", { id, side, from, to, text, caret }),
  insertLine: (id: number, side: SideName, row: number, text: string) =>
    invoke<EditInfo>("insert_line", { id, side, row, text }),
  copyRows: (id: number, start: number, end: number, to: SideName) =>
    invoke<EditInfo>("copy_rows", { id, start, end, to }),
  rowsText: (id: number, side: SideName, start: number, end: number) =>
    invoke<string>("rows_text", { id, side, start, end }),
  replaceRows: (id: number, side: SideName, start: number, end: number, text: string, replace: boolean) =>
    invoke<EditInfo>("replace_rows", { id, side, start, end, text, replace }),
  undo: (id: number) => invoke<EditInfo>("undo", { id }),
  redo: (id: number) => invoke<EditInfo>("redo", { id }),
  updateDiff: (id: number, options: DiffOptions, syntax: string | null) =>
    invoke<EditInfo>("update_diff", { id, options, syntax }),
  saveSide: (id: number, side: SideName) => invoke<EditInfo>("save_side", { id, side }),
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
