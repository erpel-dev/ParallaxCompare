// Helpers for the folder compare result tree.
import type { Node, Side, Status } from "./api";

export type Filter = "all" | "diffs" | "orphans" | Exclude<Status, "pending">;

export interface FlatRow {
  node: Node;
  path: string;
  depth: number;
  container: boolean;
  expanded: boolean;
  /** Flat view: the folder of each side's file, shown before the name. */
  folders?: [string, string];
  /** Flat view: the right file's path, for a file matched with one in another folder. */
  rightPath?: string;
  /** Flat view: a matched pair whose files are still being compared. */
  pending?: boolean;
  /** Per side: inside a folder ignored by Git, which carries the mark. */
  inIgnored?: [boolean, boolean];
}

const holds = (side: Side | null) => !!side && (side.kind === "dir" || side.kind === "archive");

/** Whether the row has entries below it: a folder or archive on either side (a file on the other still has them). */
export function isContainer(node: Node): boolean {
  return holds(node.left) || holds(node.right);
}

/** An archive not opened yet: compared as a file, its entries are loaded when it is expanded. */
export function closedArchive(node: Node): boolean {
  return !!node.unexpanded && (node.left ?? node.right)?.kind === "archive";
}

export function selfMatches(node: Node, filter: Filter): boolean {
  // Not compared yet: shown whatever it turns out to be.
  if (node.status === "pending") return true;
  switch (filter) {
    case "all":
      return true;
    case "diffs":
      return node.status !== "same" && node.status !== "similar";
    case "orphans":
      return node.status === "leftOnly" || node.status === "rightOnly";
    default:
      return node.status === filter;
  }
}

function visible(node: Node, filter: Filter, memo: Map<Node, boolean>): boolean {
  if (filter === "all") return true;
  let hit = memo.get(node);
  if (hit === undefined) {
    // A folder's aggregate status must not match instead of its descendants:
    // a "different" folder may contain only left-only files.
    hit = isContainer(node) && !closedArchive(node) && node.status !== "error" && (node.children?.length ?? 0) > 0
      ? node.children!.some((c) => visible(c, filter, memo))
      : selfMatches(node, filter);
    memo.set(node, hit);
  }
  return hit;
}

/** Loaded ancestors to open when selecting a count; unopened archives stay unopened. */
export function matchingFolders(root: Node, filter: Filter): string[] {
  const out: string[] = [];
  const memo = new Map<Node, boolean>();
  const walk = (node: Node, dir: string) => {
    for (const child of node.children ?? []) {
      if (!isContainer(child) || child.unexpanded || !visible(child, filter, memo)) continue;
      const path = joinPath(dir, child.name);
      out.push(path);
      walk(child, path);
    }
  };
  walk(root, "");
  return out;
}

export function joinPath(dir: string, name: string): string {
  return dir ? `${dir}/${name}` : name;
}

/** The rows currently shown, in display order. */
export function flatten(root: Node, expanded: Set<string>, filter: Filter): FlatRow[] {
  const out: FlatRow[] = [];
  const memo = new Map<Node, boolean>();
  const walk = (node: Node, dir: string, depth: number, inIgnored: [boolean, boolean]) => {
    for (const child of node.children ?? []) {
      if (!visible(child, filter, memo)) continue;
      const path = joinPath(dir, child.name);
      const container = isContainer(child);
      const open = container && expanded.has(path);
      out.push({ node: child, path, depth, container, expanded: open, inIgnored });
      if (open) walk(child, path, depth + 1, [inIgnored[0] || !!child.left?.ignored, inIgnored[1] || !!child.right?.ignored]);
    }
  };
  walk(root, "", 0, [false, false]);
  return out;
}

/** Paths of all folders that contain differences, to open them initially. */
export function differingFolders(root: Node, limit: number): string[] {
  const out: string[] = [];
  const walk = (node: Node, dir: string) => {
    for (const child of node.children ?? []) {
      if (out.length >= limit) return;
      // Archives not opened yet open when the user expands them.
      if (isContainer(child) && child.status === "different" && !child.unexpanded) {
        const path = joinPath(dir, child.name);
        out.push(path);
        walk(child, path);
      }
    }
  };
  walk(root, "");
  return out;
}

export function allFolders(root: Node): string[] {
  const out: string[] = [];
  const walk = (node: Node, dir: string) => {
    for (const child of node.children ?? []) {
      if (isContainer(child)) {
        const path = joinPath(dir, child.name);
        out.push(path);
        walk(child, path);
      }
    }
  };
  walk(root, "");
  return out;
}

export function countNodes(root: Node): number {
  let n = 0;
  const walk = (node: Node) => {
    for (const child of node.children ?? []) {
      n++;
      walk(child);
    }
  };
  walk(root);
  return n;
}

/**
 * Replaces the node at `path` (or removes it when `replacement` is null) and
 * recomputes the status of the folders above it.
 */
export function replaceNode(root: Node, path: string, replacement: Node | null): void {
  const parts = path.split("/");
  const chain: Node[] = [root];
  for (const part of parts.slice(0, -1)) {
    const next = chain[chain.length - 1].children?.find((c) => c.name === part);
    if (!next) return;
    chain.push(next);
  }
  const parent = chain[chain.length - 1];
  const name = parts[parts.length - 1];
  const children = parent.children ?? [];
  const index = children.findIndex((c) => c.name === name);
  if (replacement) {
    replacement.name = name;
    if (index >= 0) children[index] = replacement;
    else children.splice(insertionIndex(children, replacement), 0, replacement);
  } else if (index >= 0) {
    children.splice(index, 1);
  }
  parent.children = children;
  for (let i = chain.length - 1; i >= 1; i--) {
    const node = chain[i];
    if (holds(node.left) && holds(node.right)) {
      const children = node.children ?? [];
      node.status = children.every((c) => c.status === "same")
        ? "same"
        : children.every((c) => c.status === "same" || c.status === "similar")
          ? "similar"
          : "different";
    }
  }
}

/** Where a new child goes in the order the compare engine uses: folders first, then by name. */
function insertionIndex(children: Node[], node: Node): number {
  const key = (n: Node): [number, string, string] => [isContainer(n) ? 0 : 1, n.name.toLowerCase(), n.name];
  const k = key(node);
  const i = children.findIndex((c) => {
    const ck = key(c);
    return ck[0] > k[0] || (ck[0] === k[0] && (ck[1] > k[1] || (ck[1] === k[1] && ck[2] > k[2])));
  });
  return i < 0 ? children.length : i;
}

/**
 * The path to compare again so that a change at `path` shows: the path itself
 * when its parent's children are loaded, otherwise the deepest loaded ancestor.
 */
export function refreshTarget(root: Node, path: string): string {
  let node = root;
  let at = "";
  for (const part of path.split("/").filter(Boolean)) {
    if (node.unexpanded || !node.children) return at;
    const here = joinPath(at, part);
    const child = node.children.find((c) => c.name === part);
    // New: compared as a whole and inserted.
    if (!child) return here;
    node = child;
    at = here;
  }
  return path;
}

/** Whole archives are one copy operation, regardless of expanded/filtered children. */
export function copyFilesBelow(node: Node, path: string, left: boolean): { node: Node; path: string }[] {
  const side = left ? node.left : node.right;
  if (!side) return [];
  if (side.kind !== "dir" || node.unexpanded) return [{ node, path }];
  return (node.children ?? []).flatMap((c) => copyFilesBelow(c, joinPath(path, c.name), left));
}

export function findNode(root: Node, path: string): Node | undefined {
  let node: Node | undefined = root;
  for (const part of path.split("/").filter(Boolean)) {
    node = node?.children?.find((c) => c.name === part);
  }
  return node;
}

export interface Counts {
  files: number;
  same: number;
  similar: number;
  different: number;
  leftOnly: number;
  rightOnly: number;
  errors: number;
}

/** File counts of the current tree; stays correct after copy and delete. */
export function countFiles(root: Node): Counts {
  const c: Counts = { files: 0, same: 0, similar: 0, different: 0, leftOnly: 0, rightOnly: 0, errors: 0 };
  const walk = (node: Node) => {
    for (const child of node.children ?? []) {
      if (child.status === "pending") continue;
      if (isContainer(child) && child.status !== "error" && !closedArchive(child)) {
        walk(child);
        continue;
      }
      c.files++;
      if (child.status === "error") c.errors++;
      else c[child.status]++;
    }
  };
  walk(root);
  return c;
}

/** Counts in a flat comparison use its matched pairs, once per displayed file pair. */
export function countFlatFiles(rows: FlatRow[]): Counts {
  const c: Counts = { files: 0, same: 0, similar: 0, different: 0, leftOnly: 0, rightOnly: 0, errors: 0 };
  for (const { node, pending } of rows) {
    if (pending || node.status === "pending") continue;
    c.files++;
    if (node.status === "error") c.errors++;
    else c[node.status]++;
  }
  return c;
}

export function matchingCount(c: Counts, filter: Filter): number {
  if (filter === "all") return c.files;
  if (filter === "diffs") return c.different + c.leftOnly + c.rightOnly + c.errors;
  if (filter === "orphans") return c.leftOnly + c.rightOnly;
  return filter === "error" ? c.errors : c[filter];
}

/** One side of a row: the path, and whether it is the left side's entry. */
export interface RowSide {
  path: string;
  left: boolean;
}

/**
 * The two entries to compare when two rows are selected: preferably the first
 * row's left entry with the second row's right one, so a file renamed between
 * the sides (left only, right only) pairs up whichever order the rows are in.
 * `null` when a file would be paired with a folder.
 */
export function pairRows(a: FlatRow, b: FlatRow): [RowSide, RowSide] | null {
  const sides = (r: FlatRow) => [r.node.left && { path: r.path, left: true }, r.node.right && { path: r.path, left: false }];
  const [al, ar] = sides(a);
  const [bl, br] = sides(b);
  const pair = (al && br ? [al, br] : bl && ar ? [bl, ar] : [al ?? ar, bl ?? br]) as [RowSide | null, RowSide | null];
  if (!pair[0] || !pair[1] || a.container !== b.container) return null;
  return pair as [RowSide, RowSide];
}

/** The status of a file compared with one in another folder. */
export interface PairStatus {
  status: Node["status"];
  message: string | null;
}

function parent(path: string): string {
  const i = path.lastIndexOf("/");
  return i < 0 ? "" : path.slice(0, i);
}

/** Every file below `root` with its path; archives and folders with loaded children are walked into. */
function files(root: Node): { node: Node; path: string }[] {
  const out: { node: Node; path: string }[] = [];
  const walk = (node: Node, dir: string) => {
    for (const child of node.children ?? []) {
      const path = joinPath(dir, child.name);
      if (isContainer(child) && (child.children?.length || child.unexpanded)) walk(child, path);
      else if (!isContainer(child)) out.push({ node: child, path });
    }
  };
  walk(root, "");
  return out;
}

/** How alike two folder paths are: shared trailing segments count most, then shared leading ones. */
function likeness(a: string, b: string): number {
  const x = a ? a.split("/") : [];
  const y = b ? b.split("/") : [];
  let tail = 0;
  while (tail < Math.min(x.length, y.length) && x[x.length - 1 - tail] === y[y.length - 1 - tail]) tail++;
  let head = 0;
  while (head < Math.min(x.length, y.length) - tail && x[head] === y[head]) head++;
  return tail * 1000 + head * 10 - Math.abs(x.length - y.length);
}

/** Above this many left × right pairs of one name, every pair is not scored. */
const MAX_CANDIDATES = 20_000;

/**
 * Matching for names that many files share (`index.ts` in hundreds of folders):
 * each left file looks only at right files whose folders end the same way (the
 * last three, two or one folder names), at most 64 of them, instead of scoring
 * every pair. Files with no such partner stay unmatched.
 */
function matchByFolderSuffix(left: string[], right: string[]): [string, string][] {
  const folders = (p: string) => parent(p).split("/").filter(Boolean);
  const key = (parts: string[], k: number) => `${k}:${parts.slice(-k).join("/")}`;
  // Matched files leave their buckets, so later lookups do not step over them.
  const index = new Map<string, Set<string>>();
  const bucketsOf = new Map<string, string[]>();
  for (const r of right) {
    const parts = folders(r);
    const keys: string[] = [];
    for (let k = 1; k <= Math.min(3, parts.length); k++) {
      const name = key(parts, k);
      keys.push(name);
      const bucket = index.get(name);
      if (bucket) bucket.add(r);
      else index.set(name, new Set([r]));
    }
    bucketsOf.set(r, keys);
  }
  const pairs: [string, string][] = [];
  for (const l of [...left].sort()) {
    const parts = folders(l);
    for (let k = Math.min(3, parts.length); k >= 1; k--) {
      let best: string | null = null;
      let bestScore = -Infinity;
      let looked = 0;
      for (const r of index.get(key(parts, k)) ?? []) {
        const score = likeness(parent(l), parent(r));
        if (score > bestScore || (score === bestScore && best !== null && r < best)) [best, bestScore] = [r, score];
        if (++looked >= 64) break;
      }
      if (best !== null) {
        for (const name of bucketsOf.get(best) ?? []) index.get(name)?.delete(best);
        pairs.push([l, best]);
        break;
      }
    }
  }
  return pairs;
}

/**
 * Files only on the left matched with files of the same name only on the right,
 * as `[left path, right path]`: files at the same path are matched already, so
 * these are files that moved to another folder. Of several candidates, the one
 * whose folder is most alike wins.
 */
export function movedPairs(root: Node): [string, string][] {
  const byName = new Map<string, { left: string[]; right: string[] }>();
  for (const f of files(root)) {
    const side = f.node.status === "leftOnly" ? "left" : f.node.status === "rightOnly" ? "right" : null;
    if (!side) continue;
    let entry = byName.get(f.node.name);
    if (!entry) byName.set(f.node.name, (entry = { left: [], right: [] }));
    entry[side].push(f.path);
  }
  const pairs: [string, string][] = [];
  for (const { left, right } of byName.values()) {
    if (!left.length || !right.length) continue;
    if (left.length * right.length > MAX_CANDIDATES) {
      pairs.push(...matchByFolderSuffix(left, right));
      continue;
    }
    const candidates = left.flatMap((l) => right.map((r) => ({ l, r, score: likeness(parent(l), parent(r)) })));
    candidates.sort((a, b) => b.score - a.score || a.l.localeCompare(b.l) || a.r.localeCompare(b.r));
    const used = new Set<string>();
    for (const c of candidates) {
      if (used.has(c.l) || used.has(`>${c.r}`)) continue;
      used.add(c.l).add(`>${c.r}`);
      pairs.push([c.l, c.r]);
    }
  }
  return pairs;
}

/**
 * The rows of the flat view: every file, sorted by name and then folder, as if
 * all were in one folder. Files of `moved` share a row; until their comparison
 * is in `statuses` they show as pending.
 */
export function flatFiles(
  root: Node,
  filter: Filter,
  moved: [string, string][],
  statuses: Map<string, PairStatus>,
): FlatRow[] {
  const all = files(root);
  const rightOf = new Map(moved.map(([l, r]) => [l, r]));
  const taken = new Set(moved.map(([, r]) => r));
  const byPath = new Map(all.map((f) => [f.path, f.node]));
  const out: FlatRow[] = [];
  for (const { node, path } of all) {
    if (taken.has(path)) continue;
    const right = rightOf.get(path);
    const dir = parent(path);
    if (right === undefined) {
      if (selfMatches(node, filter)) out.push({ node, path, depth: 0, container: false, expanded: false, folders: [dir, dir] });
      continue;
    }
    const other = byPath.get(right);
    const known = statuses.get(`${path}\n${right}`);
    const pair: Node = {
      ...node,
      right: other?.right ?? null,
      status: known?.status ?? "different",
      message: known?.message ?? `Matched with ${right}`,
    };
    if (known && !selfMatches(pair, filter)) continue;
    out.push({ node: pair, path, depth: 0, container: false, expanded: false, folders: [dir, parent(right)], rightPath: right, pending: !known });
  }
  const key = (r: FlatRow) => r.node.name.toLowerCase();
  out.sort((a, b) => key(a).localeCompare(key(b)) || a.path.localeCompare(b.path));
  return out;
}

/** What a compare in progress knows so far (see `Partial` in the core). */
export interface Partial {
  listed: { dir: string; entries: { name: string; left: Side | null; right: Side | null }[] }[];
  done: { path: string; status: Status; unexpanded?: boolean; message?: string }[];
}

/**
 * Adds what a running compare found to the tree shown meanwhile: listed folders
 * get their entries, not compared yet, and finished entries their status.
 * `index` holds the nodes by path.
 */
export function mergePartial(root: Node, index: Map<string, Node>, partial: Partial): void {
  for (const { dir, entries } of partial.listed) {
    const parent = dir ? index.get(dir) : root;
    if (!parent) continue;
    parent.children = entries.map((e) => {
      const path = joinPath(dir, e.name);
      let node = index.get(path);
      if (!node) index.set(path, (node = { name: e.name, left: e.left, right: e.right, status: "pending" }));
      return node;
    });
  }
  for (const d of partial.done) {
    const node = index.get(d.path);
    if (!node) continue;
    node.status = d.status;
    node.unexpanded = d.unexpanded;
    node.message = d.message;
  }
}
