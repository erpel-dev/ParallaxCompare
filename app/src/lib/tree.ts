// Helpers for the folder compare result tree.
import type { Node } from "./api";

export type Filter = "all" | "diffs" | "same" | "orphans";

export interface FlatRow {
  node: Node;
  path: string;
  depth: number;
  container: boolean;
  expanded: boolean;
}

export function isContainer(node: Node): boolean {
  const side = node.left ?? node.right;
  return !!side && (side.kind === "dir" || side.kind === "archive");
}

function selfMatches(node: Node, filter: Filter): boolean {
  switch (filter) {
    case "all":
      return true;
    case "diffs":
      return node.status !== "same";
    case "same":
      return node.status === "same";
    case "orphans":
      return node.status === "leftOnly" || node.status === "rightOnly";
  }
}

function visible(node: Node, filter: Filter, memo: Map<Node, boolean>): boolean {
  if (filter === "all") return true;
  if (filter === "diffs") return node.status !== "same";
  let hit = memo.get(node);
  if (hit === undefined) {
    hit = selfMatches(node, filter) || (node.children ?? []).some((c) => visible(c, filter, memo));
    memo.set(node, hit);
  }
  return hit;
}

export function joinPath(dir: string, name: string): string {
  return dir ? `${dir}/${name}` : name;
}

/** The rows currently shown, in display order. */
export function flatten(root: Node, expanded: Set<string>, filter: Filter): FlatRow[] {
  const out: FlatRow[] = [];
  const memo = new Map<Node, boolean>();
  const walk = (node: Node, dir: string, depth: number) => {
    for (const child of node.children ?? []) {
      if (!visible(child, filter, memo)) continue;
      const path = joinPath(dir, child.name);
      const container = isContainer(child);
      const open = container && expanded.has(path);
      out.push({ node: child, path, depth, container, expanded: open });
      if (open) walk(child, path, depth + 1);
    }
  };
  walk(root, "", 0);
  return out;
}

/** Paths of all folders that contain differences, to open them initially. */
export function differingFolders(root: Node, limit: number): string[] {
  const out: string[] = [];
  const walk = (node: Node, dir: string) => {
    for (const child of node.children ?? []) {
      if (out.length >= limit) return;
      if (isContainer(child) && child.status === "different") {
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
    else children.push(replacement);
  } else if (index >= 0) {
    children.splice(index, 1);
  }
  parent.children = children;
  for (let i = chain.length - 1; i >= 1; i--) {
    const node = chain[i];
    if (node.left && node.right && isContainer(node)) {
      node.status = (node.children ?? []).every((c) => c.status === "same") ? "same" : "different";
    }
  }
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
  different: number;
  leftOnly: number;
  rightOnly: number;
  errors: number;
}

/** File counts of the current tree; stays correct after copy and delete. */
export function countFiles(root: Node): Counts {
  const c: Counts = { files: 0, same: 0, different: 0, leftOnly: 0, rightOnly: 0, errors: 0 };
  const walk = (node: Node) => {
    for (const child of node.children ?? []) {
      if (isContainer(child) && child.status !== "error") {
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
