<script lang="ts">
  import { splitPatterns, joinPatterns } from "./patterns";
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { ask } from "@tauri-apps/plugin-dialog";
  import { api, defaultCompareOptions, sourceText, type CompareResult, type Node, type OpResult, type PathsChanged, type SessionInfo, type Side, type SourceSpec, type Version, type Versions } from "./api";
  import type { Command } from "./commands";
  import Dropdown from "./Dropdown.svelte";
  import FilterBar, { type CountFilter } from "./FilterBar.svelte";
  import Menu, { type MenuItem, type MenuContext } from "./Menu.svelte";
  import Nav, { type NavActions } from "./Nav.svelte";
  import { notify } from "./toast.svelte";
  import type { FolderViewState } from "./viewState";
  import { loadVersions } from "./versions";
  import PathBar, { type SideEdit } from "./PathBar.svelte";
  import { scaledWheel, scrollScale, spacerHeight, windowOffset } from "./vscroll";
  import { formatSize, formatTime } from "./format";
  import {
    allFolders,
    countFiles,
    countFlatFiles,
    matchingCount,
    matchingFolders,
    selfMatches,
    countNodes,
    differingFolders,
    copyFilesBelow,
    findNode,
    flatFiles,
    flatten,
    mergePartial,
    movedPairs,
    pairRows,
    refreshTarget,
    replaceNode,
    type Filter,
    type FlatRow,
    type Partial,
    type PairStatus,
    type RowSide,
  } from "./tree";

  let {
    session,
    onOpenFile,
    onSwap,
    onCompareRows,
    edit,
    onSetBase,
    nav,
    reveal,
    restore,
    onOpenComparison,
  }: {
    session: SessionInfo;
    onOpenFile: (path: string) => void;
    onSwap: () => void;
    onCompareRows?: (a: RowSide, b: RowSide) => void;
    edit?: [SideEdit, SideEdit];
    /** Compares with these sides replaced, e.g. by a folder of the tree as the new base. */
    onSetBase?: (changes: { left?: SourceSpec; right?: SourceSpec }) => Promise<boolean>;
    nav?: NavActions;
    /** A row to select and scroll to, e.g. the file just left with "up"; `n` repeats a request. */
    reveal?: { path: string; n: number };
    /** How the comparison this one replaced was shown: open folders, selection, filters. */
    restore?: FolderViewState;
    /** Opens a comparison of two sources in a new tab. */
    onOpenComparison?: (left: SourceSpec, right: SourceSpec) => void;
  } = $props();

  /** Per side: its versions when it lies in a Git repository, for "Compare with HEAD". */
  let sideVersions = $state<[Versions | null, Versions | null]>([null, null]);
  $effect(() => {
    const id = session.id;
    void session.left.display;
    void session.right.display;
    let stale = false;
    Promise.all([true, false].map((left) => api.sideSource(id, left, "").then((s) => loadVersions(s)).catch(() => null))).then(
      ([l, r]) => !stale && (sideVersions = [l, r]),
    );
    return () => (stale = true);
  });

  /** A file of one side next to a version of it from its repository, in a new tab. */
  async function compareWithVersion(path: string, left: boolean, version: Version) {
    try {
      const spec = await api.sideSource(session.id, left, sidePath(path, left));
      onOpenComparison?.(await api.versionSpec(spec, version), spec);
    } catch (e) {
      notify(String(e));
    }
  }

  const ROW = 22;

  // Options taken over from the comparison this one replaced, if any.
  // svelte-ignore state_referenced_locally
  const initialOptions = restore?.options ?? defaultCompareOptions();
  let options = $state(initialOptions);
  let excludesText = $state(joinPatterns(initialOptions.excludes, true));
  let result = $state.raw<CompareResult | null>(null);
  // The result tree is large and mutated in place; this counter drives re-rendering.
  let version = $state(0);
  let expanded = new Set<string>();
  let filter = $state<Filter>("all");
  /** The focused row; `selection` holds all selected rows (Ctrl/Shift+click). */
  let selected = $state<string | null>(null);
  let activeSide = $state<"left" | "right">("left");
  let selection = $state(new Set<string>());
  let anchor: string | null = null;
  let running = $state(false);
  /** The tree shown while the compare runs, built from its partial results; not the result yet. */
  let provisional = $state(false);
  let provisionalIndex = new Map<string, Node>();
  let progress = $state(0);
  let error = $state("");
  let busy = $state("");
  /** A running copy or delete: what it is, and its progress. */
  let op = $state<{ label: string; done: number; total: number } | null>(null);
  /** Paths changed on disk while a compare or an operation was running. */
  let deferred = new Set<string>();
  let deferredFull = false;
  const trashName = navigator.userAgent.includes("Windows") ? "Recycle Bin" : "Trash";

  let viewport = $state<HTMLDivElement>();
  let scrollTop = $state(0);
  let viewHeight = $state(600);

  /** Files of all folders in one list, files of the same name matched across folders. */
  let flat = $state(false);
  /** Comparisons of matched files, by `left\nright` path. */
  let pairStatus = new Map<string, PairStatus>();
  const moved = $derived.by(() => {
    void version;
    return flat && result ? movedPairs(result.root) : [];
  });
  const flatRows = $derived.by(() => {
    void version;
    return flat && result ? flatFiles(result.root, "all", moved, pairStatus) : [];
  });
  const rows = $derived.by(() => {
    void version;
    if (!result) return [];
    return flat ? flatRows.filter((r) => r.pending || selfMatches(r.node, filter)) : flatten(result.root, expanded, filter);
  });
  const rowByPath = $derived(new Map(rows.map((r) => [r.path, r])));

  // Matched files are compared once they are shown.
  let comparing = false;
  $effect(() => {
    const todo = moved.filter(([l, r]) => !pairStatus.has(`${l}\n${r}`));
    if (!todo.length || comparing) return;
    comparing = true;
    const id = session.id;
    api
      .comparePairs(id, todo, $state.snapshot(options))
      .then((statuses) => {
        todo.forEach(([l, r], i) => pairStatus.set(`${l}\n${r}`, statuses[i]));
      })
      .catch((e) => (error = String(e)))
      .finally(() => {
        comparing = false;
        version++;
      });
  });

  /**
   * Loads the identical folders not walked yet (skipped by Git tree id), also those
   * inside them, so all their files show.
   */
  async function loadSkipped() {
    if (!result) return;
    for (;;) {
      const pending: string[] = [];
      const walk = (node: Node, dir: string) => {
        for (const c of node.children ?? []) {
          const p = dir ? `${dir}/${c.name}` : c.name;
          if (c.unexpanded) pending.push(p);
          else walk(c, p);
        }
      };
      walk(result.root, "");
      if (!pending.length) break;
      let loaded = 0;
      for (const p of pending) {
        busy = `Loading ${p}…`;
        try {
          const node = findNode(result.root, p);
          const fresh = await api.expand(session.id, p, $state.snapshot(options));
          if (node) {
            replaceNode(result.root, p, fresh);
            loaded++;
          }
        } catch (e) {
          error = String(e);
        }
      }
      // Nothing could be loaded: stop rather than ask again.
      if (!loaded) break;
    }
    busy = "";
  }

  /** Switches the flat view; identical folders not walked yet are loaded first, so their files show. */
  async function setFlat(on: boolean) {
    if (on) await loadSkipped();
    flat = on;
    version++;
  }

  /** The path of a row's entry on one side: a matched row has its own on the right. */
  function sidePath(path: string, left: boolean): string {
    return left ? path : (rowByPath.get(path)?.rightPath ?? path);
  }

  /** Forgets comparisons of matched files at or below `paths`. */
  function forgetPairs(paths: string[]) {
    for (const key of [...pairStatus.keys()]) {
      if (key.split("\n").some((p) => paths.some((q) => p === q || p.startsWith(q + "/") || q === ""))) pairStatus.delete(key);
    }
  }
  const counts = $derived.by(() => {
    void version;
    return result ? (flat ? countFlatFiles(flatRows) : countFiles(result.root)) : null;
  });
  const countFilters = $derived<CountFilter[]>([
    { id: "all", label: "All files", count: counts?.files ?? 0 },
    { id: "diffs", label: "All differences", count: counts ? matchingCount(counts, "diffs") : 0, title: "Changed, left-only, right-only, and error entries", tone: "different" },
    { id: "same", label: "Same", count: counts?.same ?? 0, tone: "same" },
    { id: "similar", label: "Similar", count: counts?.similar ?? 0, title: "Equal content, different bytes", tone: "similar" },
    { id: "different", label: "Different", count: counts?.different ?? 0, title: "Changed files present on both sides", tone: "different" },
    { id: "leftOnly", label: "Left only", count: counts?.leftOnly ?? 0, tone: "left" },
    { id: "rightOnly", label: "Right only", count: counts?.rightOnly ?? 0, tone: "right" },
    { id: "error", label: "Errors", count: counts?.errors ?? 0, tone: "error" },
  ]);
  const matchingFiles = $derived(counts ? matchingCount(counts, filter) : 0);

  function setFilter(id: Filter) {
    filter = id;
    menu = null;
    selected = null;
    selection = new Set();
    anchor = null;
    if (result && !flat && id !== "all") {
      for (const folder of matchingFolders(result.root, id)) expanded.add(folder);
    }
    version++;
    scrollTop = 0;
    if (viewport) viewport.scrollTop = 0;
  }

  // Huge trees scroll through a scaled spacer (see vscroll.ts); `vTop` is the
  // scroll position in row pixels.
  const scale = $derived(scrollScale(rows.length * ROW, viewHeight));
  const vTop = $derived(scrollTop * scale);
  const first = $derived(Math.max(0, Math.floor(vTop / ROW) - 10));
  const last = $derived(Math.min(rows.length, first + Math.ceil(viewHeight / ROW) + 20));
  const selectedIndex = $derived(selected === null ? -1 : rows.findIndex((r) => r.path === selected));
  const selectedRow = $derived(selectedIndex >= 0 ? rows[selectedIndex] : null);
  /** With two rows selected: the entries "Compare these" compares. */
  const pair = $derived.by(() => {
    if (selection.size !== 2) return null;
    const picked = rows.filter((r) => selection.has(r.path));
    return picked.length === 2 ? pairRows(picked[0], picked[1]) : null;
  });

  // Reveal a row once the comparison is there; folders on the way are opened.
  let revealed = 0;
  $effect(() => {
    if (!reveal || !result || reveal.n === revealed) return;
    revealed = reveal.n;
    revealPath(reveal.path);
  });

  async function revealPath(path: string) {
    if (!result) return;
    const parts = path.split("/");
    for (let i = 1; i < parts.length; i++) {
      const folder = parts.slice(0, i).join("/");
      if (!expanded.has(folder)) await toggle(folder);
    }
    if (filter !== "all" && !rows.some((r) => r.path === path)) {
      filter = "all";
      version++;
    }
    await tick();
    const index = rows.findIndex((r) => r.path === path);
    if (index < 0) return;
    selectOnly(path);
    await ensureVisible(index);
    viewport?.focus();
  }

  // ---- File patterns ----
  let patternText = $state(joinPatterns(initialOptions.includes));
  let patternInput = $state<HTMLInputElement>();
  let patternTimer: ReturnType<typeof setTimeout> | undefined;

  /** Compares again with the patterns typed, when they changed. */
  function applyPatterns() {
    clearTimeout(patternTimer);
    let list: string[];
    try { list = splitPatterns(patternText); }
    catch (e) { error = String(e); return; }
    if (list.join("\n") === options.includes.join("\n")) return;
    options.includes = list;
    run();
  }

  function onPatternKey(e: KeyboardEvent) {
    if (e.key === "Enter") applyPatterns();
    else if (e.key === "Escape") {
      patternText = "";
      applyPatterns();
    }
  }

  /**
   * Compares again. A refresh (`invalidate`) keeps showing the old result until the
   * new one is there; otherwise the entries show as the compare finds them.
   */
  /** What the view shows, to show the same after one side was switched. */
  export function viewState(): FolderViewState {
    return {
      expanded: [...expanded],
      selected,
      filter,
      flat,
      scrollTop: viewport?.scrollTop ?? 0,
      options: $state.snapshot(options),
    };
  }

  /** The state to take over once the first comparison is done. */
  // svelte-ignore state_referenced_locally
  let restoring: FolderViewState | null = restore ?? null;
  if (restoring) {
    filter = restoring.filter;
    flat = restoring.flat;
  }

  /** Opens again what was open, where it still exists, and selects and scrolls as before. */
  async function finishRestore(state: FolderViewState) {
    if (!result) return;
    for (const path of [...state.expanded].sort((a, b) => a.split("/").length - b.split("/").length)) {
      const node = findNode(result.root, path);
      if (!node) {
        expanded.delete(path);
      } else if (node.unexpanded) {
        expanded.delete(path);
        await toggle(path);
      }
    }
    version++;
    await tick();
    if (viewport) viewport.scrollTop = state.scrollTop;
    // The selected row, or the nearest folder above it that still exists.
    let path = state.selected;
    while (path && !rows.some((r) => r.path === path)) path = path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : null;
    if (path) selectOnly(path);
  }

  async function run(invalidate = false) {
    running = true;
    error = "";
    progress = 0;
    if (!invalidate) {
      // Folders the user opens meanwhile stay open, and those open before a side was switched.
      expanded = new Set(restoring?.expanded ?? []);
      provisionalIndex = new Map();
      provisional = true;
      result = {
        root: { name: "", left: null, right: null, status: "pending", children: [] },
        summary: { files: 0, same: 0, similar: 0, different: 0, leftOnly: 0, rightOnly: 0, errors: 0, identicalSubtrees: 0, elapsedMs: 0 },
      };
      version++;
    }
    try {
      const r = await api.runCompare(session.id, $state.snapshot(options), invalidate);
      // Open folders with differences, but keep huge trees collapsed.
      const opened = provisional ? [...expanded] : [];
      expanded = new Set([...differingFolders(r.root, countNodes(r.root) > 20000 ? 0 : 400), ...opened]);
      pairStatus = new Map();
      provisional = false;
      result = r;
      version++;
      if (restoring) {
        const state = restoring;
        restoring = null;
        await finishRestore(state);
      }
    } catch (e) {
      if (String(e) !== "cancelled") error = String(e);
      if (provisional) {
        provisional = false;
        result = null;
        version++;
      }
    } finally {
      running = false;
    }
    // Changes that came in during the compare are in the new result already.
    deferred.clear();
    deferredFull = false;
  }

  onMount(() => {
    run();
    const unlisten = [
      listen<{ session: number; files: number; partial?: Partial }>("compare-progress", (e) => {
        if (e.payload.session !== session.id) return;
        progress = e.payload.files;
        if (e.payload.partial && provisional && running && result) {
          mergePartial(result.root, provisionalIndex, e.payload.partial);
          version++;
        }
      }),
      listen<{ session: number; done: number; total: number }>("op-progress", (e) => {
        if (e.payload.session === session.id && op) op = { ...op, done: e.payload.done, total: e.payload.total };
      }),
      listen<PathsChanged>("paths-changed", (e) => {
        if (e.payload.session !== session.id) return;
        if (e.payload.full) deferredFull = true;
        for (const p of e.payload.paths) deferred.add(p);
        flushChanges();
      }),
    ];
    return () => unlisten.forEach((u) => u.then((f) => f()));
  });

  function applyExcludes() {
    let next: string[];
    try { next = splitPatterns(excludesText, true); }
    catch (e) { error = String(e); return; }
    if (next.join("\n") !== options.excludes.join("\n")) {
      options.excludes = next;
      run();
    }
  }

  async function toggle(path: string) {
    if (!result) return;
    if (expanded.has(path)) {
      expanded.delete(path);
    } else {
      const node = findNode(result.root, path);
      if (node?.unexpanded) {
        busy = `Loading ${path}…`;
        try {
          // An archive compared as a file may turn out to have the same entries.
          replaceNode(result.root, path, await api.expand(session.id, path, $state.snapshot(options)));
        } catch (e) {
          error = String(e);
        } finally {
          busy = "";
        }
      }
      expanded.add(path);
    }
    version++;
  }

  async function setAll(open: boolean) {
    if (!result) return;
    if (open) await loadSkipped();
    expanded = open ? new Set(allFolders(result.root)) : new Set();
    version++;
  }

  function activate(path: string, container: boolean) {
    selectOnly(path);
    const right = rowByPath.get(path)?.rightPath;
    if (container) toggle(path);
    else if (right && onCompareRows) onCompareRows({ path, left: true }, { path: right, left: false });
    else onOpenFile(path);
  }

  function selectOnly(path: string) {
    selected = anchor = path;
    selection = new Set([path]);
  }

  function click(e: MouseEvent, path: string) {
    if (e.shiftKey && anchor) {
      selectRange(anchor, path);
    } else if (e.ctrlKey || e.metaKey) {
      const next = new Set(selection);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      selection = next;
      selected = anchor = path;
    } else {
      selectOnly(path);
    }
  }

  function selectRange(from: string, to: string) {
    const a = rows.findIndex((r) => r.path === from);
    const b = rows.findIndex((r) => r.path === to);
    if (a < 0 || b < 0) return selectOnly(to);
    selection = new Set(rows.slice(Math.min(a, b), Math.max(a, b) + 1).map((r) => r.path));
    selected = to;
  }

  async function ensureVisible(index: number) {
    await tick();
    if (!viewport) return;
    const top = index * ROW;
    const at = viewport.scrollTop * scale;
    if (top < at + ROW) viewport.scrollTop = Math.max(0, top - ROW) / scale;
    else if (top + ROW > at + viewHeight) viewport.scrollTop = (top + 2 * ROW - viewHeight) / scale;
  }

  function onKey(e: KeyboardEvent) {
    if (!rows.length) return;
    let index = selectedIndex;
    const row = selectedRow;
    switch (e.key) {
      case "ArrowDown":
        index = Math.min(rows.length - 1, index + 1);
        break;
      case "ArrowUp":
        index = Math.max(0, index - 1);
        break;
      case "PageDown":
        index = Math.min(rows.length - 1, index + Math.floor(viewHeight / ROW));
        break;
      case "PageUp":
        index = Math.max(0, index - Math.floor(viewHeight / ROW));
        break;
      case "Home":
        index = 0;
        break;
      case "End":
        index = rows.length - 1;
        break;
      case "ArrowRight":
        if (row?.container && !row.expanded) toggle(row.path);
        e.preventDefault();
        return;
      case "ArrowLeft":
        if (row?.container && row.expanded) toggle(row.path);
        else if (row && row.depth > 0) {
          const parent = row.path.slice(0, row.path.lastIndexOf("/"));
          index = rows.findIndex((r) => r.path === parent);
        }
        break;
      case "F5":
        if (!running) run(true);
        e.preventDefault();
        return;
      case "Enter":
        if (row) activate(row.path, row.container);
        e.preventDefault();
        return;
      case "ContextMenu":
      case "F10":
        if (e.key === "F10" && !e.shiftKey) return;
        if (row && viewport) {
          const r = viewport.getBoundingClientRect();
          rowMenu(row.path, activeSide, r.left + (activeSide === "left" ? 40 : r.width / 2 + 40), r.top + selectedIndex * ROW - vTop + ROW);
        }
        e.preventDefault();
        return;
      case "Delete":
        if (canDelete[1] && !canDelete[0]) remove(false);
        else if (canDelete[0] && !canDelete[1]) remove(true);
        e.preventDefault();
        return;
      case "a":
        if (!(e.ctrlKey || e.metaKey)) return;
        selection = new Set(rows.map((r) => r.path));
        e.preventDefault();
        return;
      default:
        return;
    }
    e.preventDefault();
    if (index >= 0) {
      if (e.shiftKey && anchor) selectRange(anchor, rows[index].path);
      else selectOnly(rows[index].path);
      ensureVisible(index);
    }
  }

  /** The selected paths, in display order, without those inside another selected folder. */
  const targets = $derived.by(() => {
    void version;
    const paths = rows.filter((r) => selection.has(r.path)).map((r) => r.path);
    return paths.filter((p) => !paths.some((q) => p.startsWith(q + "/")));
  });

  /** Paths of `targets` present on one side. */
  function present(left: boolean): string[] {
    if (!result) return [];
    return targets.filter((p) => {
      const node = flat ? rowByPath.get(p)?.node : findNode(result!.root, p);
      return !!(left ? node?.left : node?.right);
    });
  }

  function applyResult(paths: string[], r: OpResult) {
    if (!result) return;
    forgetPairs(paths);
    paths.forEach((p, i) => replaceNode(result!.root, p, r.nodes[i]));
    version++;
    if (r.error && r.error !== "cancelled") error = r.error;
  }

  /** Runs a copy or delete, showing its progress, then the refreshed rows. */
  async function runOp(label: string, paths: string[], call: () => Promise<OpResult>): Promise<OpResult | null> {
    op = { label, done: 0, total: 0 };
    error = "";
    try {
      const r = await call();
      applyResult(paths, r);
      return r;
    } catch (e) {
      error = String(e);
      return null;
    } finally {
      op = null;
      flushChanges();
    }
  }

  function list(paths: string[], limit = 12): string {
    const shown = paths.slice(0, limit).map((p) => `  ${p}`);
    if (paths.length > limit) shown.push(`  … and ${paths.length - limit} more`);
    return shown.join("\n");
  }

  async function copy(toRight: boolean) {
    if (!result) return;
    // What exists on the side copied from.
    const all = present(toRight);
    // Files matched across folders are copied over their match.
    const pairs = all.map((p) => rowByPath.get(p)).filter((r): r is FlatRow & { rightPath: string } => !!r?.rightPath && !!r.node.left && !!r.node.right);
    const paths = all.filter((p) => !pairs.some((r) => r.path === p));
    if (pairs.length) {
      const side = toRight ? "right" : "left";
      const changed = pairs.filter((r) => r.node.status !== "same");
      if (changed.length) {
        const names = changed.map((r) => (toRight ? `${r.path} → ${r.rightPath}` : `${r.rightPath} → ${r.path}`));
        const ok = await ask(`Copying replaces ${changed.length} matched file${changed.length > 1 ? "s" : ""} on the ${side} side:\n${list(names)}`, {
          title: "Replace files",
          kind: "warning",
          okLabel: "Replace",
          cancelLabel: "Cancel",
        });
        if (!ok) return;
      }
      for (const r of pairs) {
        const [from, to] = toRight ? [r.path, r.rightPath] : [r.rightPath, r.path];
        await runOp(`Copying ${from}`, [to], () => api.copyFileAs(session.id, from, to, toRight, $state.snapshot(options)));
      }
    }
    if (!paths.length) return;
    // Files on the target side that the copy replaces, and those newer than their source.
    const archives: string[] = [];
    const replaced: string[] = [];
    const newer: string[] = [];
    for (const p of paths) {
      for (const f of copyFilesBelow(findNode(result.root, p)!, p, toRight)) {
        const [src, dst] = toRight ? [f.node.left, f.node.right] : [f.node.right, f.node.left];
        if (src?.kind === "archive") archives.push(f.path);
        if (!src || !dst || (f.node.status === "same" && src.kind !== "archive")) continue;
        replaced.push(f.path);
        if (src.mtime !== null && dst.mtime !== null && dst.mtime > src.mtime) newer.push(f.path);
      }
    }
    const side = toRight ? "right" : "left";
    if (replaced.length || archives.length) {
      const message =
        (replaced.length ? `Copying replaces ${replaced.length} file${replaced.length > 1 ? "s" : ""} on the ${side} side:\n${list(replaced)}` : "") +
        (archives.length ? `\n\nArchives are copied whole, including entries hidden by file patterns, exclusions or status filters:\n${list(archives)}` : "") +
        (newer.length ? `\n\n${newer.length} of them ${newer.length > 1 ? "are" : "is"} newer than the copy:\n${list(newer, 6)}` : "");
      const ok = await ask(message, { title: "Replace files", kind: "warning", okLabel: "Replace", cancelLabel: "Cancel" });
      if (!ok) return;
    }
    const what = paths.length === 1 ? paths[0] : `${paths.length} items`;
    await runOp(`Copying ${what}`, paths, () => api.copyEntries(session.id, paths, toRight, $state.snapshot(options)));
  }

  /** Whether `path` lies inside an archive on that side, where deleting is for good. */
  function insideArchive(path: string, left: boolean): boolean {
    const info = left ? session.left : session.right;
    if (info.kind === "archive") return true;
    const parts = path.split("/");
    let node = result?.root;
    for (const part of parts.slice(0, -1)) {
      node = node?.children?.find((c) => c.name === part);
      if ((left ? node?.left : node?.right)?.kind === "archive") return true;
    }
    return false;
  }

  async function remove(left: boolean) {
    const paths = present(left).map((p) => sidePath(p, left));
    if (!paths.length) return;
    const side = left ? "left" : "right";
    const what = paths.length === 1 ? `"${paths[0]}"` : `${paths.length} items`;
    const inArchive = paths.filter((p) => insideArchive(p, left));
    const message = inArchive.length
      ? `Delete ${what} from the ${side} side?\n\n${inArchive.length === paths.length ? "They are" : `${inArchive.length} of them are`} inside an archive and are deleted for good; this cannot be undone.${inArchive.length < paths.length ? ` The others go to the ${trashName}.` : ""}`
      : `Move ${what} from the ${side} side to the ${trashName}?`;
    const ok = await ask(message, { title: "Delete", kind: "warning", okLabel: inArchive.length ? "Delete" : `Move to ${trashName}`, cancelLabel: "Cancel" });
    if (!ok) return;
    const r = await runOp(`Deleting ${what}`, paths, () =>
      api.deleteEntries(session.id, paths, left, false, $state.snapshot(options)),
    );
    if (!r?.notTrashed.length) return;
    const again = await ask(
      `These could not be moved to the ${trashName} (on a network drive, for example):\n${list(r.notTrashed)}\n\nDelete them permanently? This cannot be undone.`,
      { title: "Delete permanently", kind: "warning", okLabel: "Delete permanently", cancelLabel: "Keep them" },
    );
    if (again) {
      const rest = r.notTrashed;
      await runOp(`Deleting ${rest.length} items`, rest, () =>
        api.deleteEntries(session.id, rest, left, true, $state.snapshot(options)),
      );
    }
  }

  /** Brings the rows of changed paths up to date, or compares again when too much changed. */
  async function flushChanges() {
    if (!result || running || op || (!deferred.size && !deferredFull)) return;
    menu = null;
    if (deferredFull) {
      deferredFull = false;
      deferred.clear();
      run(true);
      return;
    }
    const root = result.root;
    const targets = [...new Set([...deferred].map((p) => refreshTarget(root, p)))];
    deferred.clear();
    if (targets.includes("")) {
      run(true);
      return;
    }
    // Only the outermost of nested targets.
    const outer = targets.filter((p) => !targets.some((q) => p.startsWith(q + "/")));
    forgetPairs(outer);
    try {
      const nodes = await api.refreshPaths(session.id, outer, $state.snapshot(options));
      if (result?.root !== root) return;
      outer.forEach((p, i) => replaceNode(root, p, nodes[i]));
      // A removed last match may leave empty ancestors. Ask the engine only for
      // those folders: patterns may explicitly include an empty folder by name.
      let removed = outer.filter((_, i) => nodes[i] === null);
      while (options.includes.length && removed.length) {
        const parents = [...new Set(removed.map((p) => p.split("/").slice(0, -1).join("/")))].filter((p) => {
          const node = p ? findNode(root, p) : undefined;
          return node && !node.unexpanded && !node.children?.length;
        });
        if (!parents.length) break;
        const fresh = await api.refreshPaths(session.id, parents, $state.snapshot(options));
        if (result?.root !== root) return;
        parents.forEach((p, i) => replaceNode(root, p, fresh[i]));
        removed = parents.filter((_, i) => fresh[i] === null);
      }
      version++;
    } catch (e) {
      error = String(e);
    }
    flushChanges();
  }

  let pathbars = $state<PathBar[]>([]);
  let menu = $state<{ x: number; y: number; items: MenuItem[]; context: MenuContext } | null>(null);
  const optionsChanged = $derived.by(() => {
    const d = defaultCompareOptions();
    return (
      options.mode !== d.mode ||
      options.ignoreLineEndings !== d.ignoreLineEndings ||
      options.compareContents !== d.compareContents ||
      options.verifyChecksums !== d.verifyChecksums ||
      options.excludes.join("\n") !== d.excludes.join("\n")
    );
  });

  /** Opens or reveals the file on one side of a row. */
  async function onSide(path: string, left: boolean, action: "open" | "reveal") {
    try {
      const spec = await api.sideSource(session.id, left, path);
      await api.openSource(spec, action === "reveal");
    } catch (e) {
      notify(String(e));
    }
  }

  /** Makes the folder at `path` the top folder of the given sides. */
  async function setBase(path: string, sides: ("left" | "right")[]) {
    if (!onSetBase) return;
    try {
      const changes: { left?: SourceSpec; right?: SourceSpec } = {};
      for (const side of sides) {
        changes[side] = await api.sideSource(session.id, side === "left", sidePath(path, side === "left"));
      }
      await onSetBase(changes);
    } catch (e) {
      notify(String(e));
    }
  }

  /** The clicked side supplies files; the opposite side is only a copy destination. */
  function rowMenu(path: string, side: "left" | "right", x: number, y: number) {
    if (!selection.has(path)) selectOnly(path);
    activeSide = side;
    const row = rows.find((r) => r.path === path);
    if (!row) return;
    const left = side === "left";
    const other = left ? "right" : "left";
    const source = left ? session.left : session.right;
    const destination = left ? session.right : session.left;
    const entry = left ? row.node.left : row.node.right;
    const paths = present(left);
    const incoming = present(!left);
    const n = paths.length;
    const multiple = selection.size > 1;
    const what = (n: number) => `${n} item${n === 1 ? "" : "s"}`;
    const kind = entry?.kind === "dir" ? "folder" : "file";
    const items: MenuItem[] = [
      row.container
        ? { label: row.expanded ? "Collapse" : "Expand", keys: row.expanded ? "←" : "→", run: () => toggle(path) }
        : { label: multiple ? "Open clicked comparison" : "Open comparison", detail: multiple ? path : undefined, keys: "Enter", run: () => onOpenFile(path) },
    ];
    if (pair && onCompareRows) {
      const [a, b] = pair;
      items.push({ label: "Compare these two", run: () => onCompareRows(a, b) });
    }
    if (n) {
      items.push("separator", {
        label: `${multiple ? `Copy ${what(n)}` : "Copy"} to ${other} ${left ? "→" : "←"}`,
        detail: !destination.writable ? `The ${other} source is read-only` : `From ${side} to ${other}`,
        tone: "primary", disabled: !!op || !destination.writable, run: () => copy(left),
      });
      if (onSetBase && entry && (entry.kind === "dir" || entry.kind === "archive") && !multiple) {
        items.push("separator", { label: `Set as ${side} base folder`, run: () => setBase(path, [side]) });
        const opposite = left ? row.node.right : row.node.left;
        if (opposite && (opposite.kind === "dir" || opposite.kind === "archive")) {
          items.push({ label: "Set as base folder on both sides", run: () => setBase(path, ["left", "right"]) });
        }
      }
      items.push("separator");
      if (!multiple && entry) {
        items.push(
          { label: "Open in default app", run: () => onSide(sidePath(path, left), left, "open") },
          { label: "Show in file manager", run: () => onSide(sidePath(path, left), left, "reveal") },
        );
      }
      items.push({ label: multiple ? `Copy ${n} path${n === 1 ? "" : "s"}` : "Copy path", run: () => copyPaths(paths.map((p) => sidePath(p, left)), left) });
      const repo = sideVersions[left ? 0 : 1];
      if (repo && !multiple && entry?.kind === "file" && onOpenComparison) {
        items.push("separator", {
          label: "Compare with HEAD",
          detail: `This ${side} file next to its last commit${repo.head ? ` on ${repo.head}` : ""}`,
          run: () => compareWithVersion(path, left, { type: "rev", rev: "HEAD" }),
        });
        if (repo.upstream) {
          const upstream = repo.upstream.name;
          items.push({ label: `Compare with ${upstream}`, detail: "The upstream branch, as last fetched", run: () => compareWithVersion(path, left, { type: "rev", rev: upstream }) });
        }
      }
      items.push("separator", {
        label: multiple ? `Delete ${what(n)} on ${side}…` : `Delete ${side} ${kind}…`,
        detail: source.writable ? undefined : "This source is read-only",
        tone: "danger", disabled: !!op || !source.writable, run: () => remove(left),
      });
    } else if (incoming.length) {
      items.push("separator", {
        label: multiple ? `Copy ${what(incoming.length)} from ${other} here ${left ? "←" : "→"}` : `Copy ${other} ${row.container ? "folder" : "file"} here ${left ? "←" : "→"}`,
        detail: source.writable ? `Create ${multiple ? "missing items" : row.container ? "the missing folder" : "the missing file"} on the ${side}` : `The ${side} source is read-only`,
        tone: "primary", disabled: !!op || !source.writable, run: () => copy(!left),
      });
    }
    const filename = sidePath(path, left).split("/").pop() ?? path;
    const scope = multiple
      ? targets.length !== selection.size ? `${what(n)} on the ${side} · ${selection.size} rows selected (includes nested entries)`
        : `${n} of ${targets.length} selected items exist on the ${side}`
      : entry ? "" : `Missing on the ${side}`;
    const shownPath = multiple && n === 1 ? paths[0] : path;
    menu = { x, y, items, context: {
      side, title: multiple ? `${what(n)} on the ${side}` : filename,
      detail: [multiple && n !== 1 ? source.display : `${source.display} › ${sidePath(shownPath, left)}`, scope].filter(Boolean).join("\n"),
      badge: !n ? "Missing here" : source.writable ? "Editable" : "Read-only",
    } };
  }

  async function copyPaths(paths: string[], left: boolean) {
    try {
      const specs = await Promise.all(paths.map((p) => api.sideSource(session.id, left, p)));
      await navigator.clipboard.writeText(specs.map(sourceText).join("\n"));
      notify(paths.length === 1 ? "Path copied" : `${paths.length} paths copied`, "info");
    } catch (e) {
      notify(String(e));
    }
  }

  /** What this view offers in the command palette. */
  export function commands(): Command[] {
    const group = "Folder";
    const filters = [...countFilters.map((f) => [f.id as Filter, f.label] as const), ["orphans", "Only on one side"] as const];
    return [
      { group, title: "Compare again", keys: "F5", enabled: !running, run: () => run(true) },
      { group, title: "Swap left and right", run: onSwap },
      {
        group,
        title: "Compare the two selected rows",
        enabled: !!pair && !!onCompareRows,
        run: () => pair && onCompareRows?.(pair[0], pair[1]),
      },
      ...filters.map(([id, label]) => ({
        group,
        title: `Show: ${label}`,
        run: () => setFilter(id),
      })),
      { group, title: "Filter files by name pattern", run: () => patternInput?.focus() },
      { group, title: "Expand all folders", run: () => setAll(true) },
      { group, title: "Collapse all folders", run: () => setAll(false) },
      { group, title: flat ? "Show folders" : "Flat list: all files without folders", run: () => setFlat(!flat) },
      { group, title: "Copy selection to the right", enabled: canCopy[1] && !op, run: () => copy(true) },
      { group, title: "Copy selection to the left", enabled: canCopy[0] && !op, run: () => copy(false) },
      { group, title: "Delete selection on the left", enabled: canDelete[0] && !op, run: () => remove(true) },
      { group, title: "Delete selection on the right", enabled: canDelete[1] && !op, run: () => remove(false) },
      { group, title: "Select all rows", keys: "Ctrl+A", run: () => (selection = new Set(rows.map((r) => r.path))) },
      {
        group,
        title: options.mode === "content" ? "Compare by size and date (faster)" : "Compare by content",
        run: () => {
          options.mode = options.mode === "content" ? "quick" : "content";
          run();
        },
      },
      {
        group,
        title: `${options.ignoreLineEndings ? "Stop ignoring" : "Ignore"} CRLF/LF differences`,
        run: () => {
          options.ignoreLineEndings = !options.ignoreLineEndings;
          run();
        },
      },
      {
        group,
        title: `${options.compareContents ? "Stop marking" : "Mark"} documents and images with equal content as similar`,
        run: () => {
          options.compareContents = !options.compareContents;
          run();
        },
      },
      ...pathbars.flatMap((bar) => bar?.commands() ?? []),
    ];
  }

  const canCopy = $derived.by(() => {
    void version;
    // Not on the tree shown during a compare: what a folder holds is not known yet.
    if (provisional) return [false, false];
    return [session.left.writable && present(false).length > 0, session.right.writable && present(true).length > 0];
  });
  const canDelete = $derived.by(() => {
    void version;
    if (provisional) return [false, false];
    return [session.left.writable && present(true).length > 0, session.right.writable && present(false).length > 0];
  });

  const glyph = { same: "=", similar: "≈", different: "≠", leftOnly: "◀", rightOnly: "▶", error: "!", pending: "…" } as const;

  function icon(side: Side): string {
    switch (side.kind) {
      case "dir":
        return "📁";
      case "archive":
        return "📦";
      case "symlink":
        return "🔗";
      case "submodule":
        return "🧩";
      default:
        return "📄";
    }
  }
</script>

<div class="view">
  <div class="toolbar">
    {#if nav}<Nav {nav} /><span class="sep"></span>{/if}
    <button onclick={() => run(true)} disabled={running} title="Compare again (F5)">⟳ Refresh</button>
    <button onclick={onSwap} title="Swap left and right">⇄ Swap</button>
    <button
      disabled={!pair || !onCompareRows}
      onclick={() => pair && onCompareRows?.(pair[0], pair[1])}
      title="Compare the two selected rows with each other, e.g. a file renamed between the sides (select with Ctrl+click)"
    >⇆ Compare these</button>
    <span class="sep"></span>
    <span class="patterns" class:active={options.includes.length > 0}>
      <input
        bind:this={patternInput}
        bind:value={patternText}
        class="mono"
        placeholder="Files: *.java *.xml"
        spellcheck="false"
        title={"Compare only files matching these patterns, separated by spaces, commas or semicolons, e.g. *.java *hello*w*c.\n" +
          "* matches any text, ? one character, [abc] one of them, {java,kt} either. A pattern with / matches the path (src/**/*.rs); a matching folder takes all files in it. Case does not matter.\n" +
          'Quote names containing separators, e.g. "report final.txt" or "report,final.txt". Folders without matching files are left out. Enter applies at once, Esc clears.'}
        oninput={() => {
          clearTimeout(patternTimer);
          patternTimer = setTimeout(applyPatterns, 500);
        }}
        onkeydown={onPatternKey}
      />
      {#if patternText}<button
          class="clear"
          title="Show all files"
          onclick={() => {
            patternText = "";
            applyPatterns();
          }}>×</button
        >{/if}
    </span>
    <span class="sep"></span>
    <button onclick={() => setAll(true)} disabled={flat} title="Expand all folders">Expand</button>
    <button onclick={() => setAll(false)} disabled={flat} title="Collapse all folders">Collapse</button>
    <button class:on={flat} onclick={() => setFlat(!flat)} title="All files in one list, without folders; files of the same name are matched across folders">Flat</button>
    <span class="sep"></span>
    <button disabled={!canCopy[1] || !!op} onclick={() => copy(true)} title="Copy the selection to the right side">Copy →</button>
    <button disabled={!canCopy[0] || !!op} onclick={() => copy(false)} title="Copy the selection to the left side">← Copy</button>
    <button disabled={!canDelete[0] || !!op} onclick={() => remove(true)} title="Delete the selection on the left side">Delete ◀</button>
    <button disabled={!canDelete[1] || !!op} onclick={() => remove(false)} title="Delete the selection on the right side">Delete ▶</button>
    <span class="grow"></span>
    <Dropdown label="Options" title="Compare rule, line endings, excluded files" changed={optionsChanged}>
      <div class="field">
        <span>Compare by</span>
        <select bind:value={options.mode} onchange={() => run()}>
          <option value="content">Content</option>
          <option value="quick">Size &amp; date (faster)</option>
        </select>
      </div>
      <label><input type="checkbox" bind:checked={options.ignoreLineEndings} onchange={() => run()} /> Ignore CRLF/LF differences</label>
      <label title="Documents (PDF, Word, Excel, PowerPoint, Visio, OpenDocument) with the same text and images with the same pixels are marked ≈ instead of ≠">
        <input type="checkbox" bind:checked={options.compareContents} onchange={() => run()} /> Equal content counts as similar (≈)
      </label>
      <label title="Zip and 7z entries with the same CRC32 and size are taken as equal without decompressing them. Different data shares a CRC32 only by a one-in-four-billion chance or when made to. Verifying decompresses every such entry, which takes seconds for large solid 7z archives.">
        <input type="checkbox" bind:checked={options.verifyChecksums} onchange={() => run()} /> Verify archive entries byte for byte
      </label>
      <div class="field">
        <span>Exclude (globs, comma separated)</span>
        <input
          type="text"
          class="mono"
          bind:value={excludesText}
          onkeydown={(e) => e.key === "Enter" && applyExcludes()}
          onblur={applyExcludes}
          placeholder="*.o, build/**"
          title={'Case-sensitive globs. Commas outside braces, classes and quotes separate patterns. Quote names containing commas, e.g. "report,final.txt".'}
        />
      </div>
    </Dropdown>
  </div>

  <FilterBar items={countFilters} value={filter} onSelect={(id) => setFilter(id as Filter)} disabled={!result}
    hint={running ? "Totals update during comparison" : flatRows.some((r) => r.pending) ? "Matched files are still being compared" : ""} />
  {#if result && filter !== "all"}
    <div class="filter-scope">
      <strong>{countFilters.find((f) => f.id === filter)?.label ?? "Only on one side"}</strong>
      <span>· {matchingFiles.toLocaleString()} matching files · {flat ? "Flat file list" : "Parent folders remain visible"}</span>
      <button onclick={() => setFilter("all")}>Show all</button>
    </div>
  {/if}

  <div class="pathbars">
    <PathBar bind:this={pathbars[0]} info={session.left} edit={edit?.[0]} title="Left" active={activeSide === "left"} />
    <PathBar bind:this={pathbars[1]} info={session.right} edit={edit?.[1]} title="Right" active={activeSide === "right"} />
  </div>

  <div class="head">
    <div class="side"><span>Name</span><span class="num">Size</span><span class="num">Modified</span></div>
    <div class="mid"></div>
    <div class="side"><span>Name</span><span class="num">Size</span><span class="num">Modified</span></div>
  </div>

  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
    class="viewport"
    bind:this={viewport}
    bind:clientHeight={viewHeight}
    onscroll={(e) => (scrollTop = e.currentTarget.scrollTop)}
    onwheel={(e) => viewport && scaledWheel(viewport, scale, e, ROW) && e.preventDefault()}
    onkeydown={onKey}
    tabindex="0"
    role="tree"
  >
    {#if result && rows.length === 0 && !running}
      <div class="empty">{filter !== "all" ? "Nothing matches the current filter." : result.root.status === "same" ? "Both sides are identical." : "No files to compare."}</div>
    {/if}
    <div class="spacer" style:height="{spacerHeight(rows.length * ROW)}px">
      <div class="window" style:transform="translateY({windowOffset(scrollTop, scale, first, ROW)}px)">
        {#each rows.slice(first, last) as row (row.path)}
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <div
            class="row {row.node.status}"
            class:sel={selection.has(row.path)}
            class:focus={row.path === selected}
            role="treeitem"
            aria-selected={selection.has(row.path)}
            aria-expanded={row.container ? row.expanded : undefined}
            tabindex="-1"
            onclick={(e) => {
              click(e, row.path);
              // A focused row would lose the focus for good once scrolled out of the page.
              viewport?.focus({ preventScroll: true });
            }}
            ondblclick={() => activate(row.path, row.container)}
            title={row.node.message ?? ""}
          >
            {#each [row.node.left, row.node.right] as side, i (i)}
              {#if i === 1}
                <div class="mid">{row.pending ? "…" : glyph[row.node.status]}{row.rightPath ? "↷" : ""}</div>
              {/if}
              {#if side}
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <div class="side" class:ignored={side.ignored} data-side={i === 0 ? "left" : "right"} onmousedown={() => (activeSide = i === 0 ? "left" : "right")} oncontextmenu={(e) => { e.preventDefault(); rowMenu(row.path, i === 0 ? "left" : "right", e.clientX, e.clientY); }}>
                  <span class="name" style:padding-left="{row.depth * 16}px">
                    {#if row.container}
                      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                      <span class="chev" onclick={(e) => { e.stopPropagation(); toggle(row.path); }}>{row.expanded ? "▾" : "▸"}</span>
                    {:else}
                      <span class="chev"></span>
                    {/if}
                    <span class="icon">{icon(side)}</span>{#if row.folders?.[i]}<span class="dir">{row.folders[i]}/</span>{/if}{i === 1 && row.rightPath ? row.rightPath.split("/").pop() : row.node.name}{#if side.ignored && !row.inIgnored?.[i]}<span class="ignored-tag" title="Ignored by Git: a .gitignore rule matches it, and it is not tracked">ignored</span>{/if}
                  </span>
                  <span class="num">{side.kind === "dir" ? "" : formatSize(side.size)}</span>
                  <span class="num">{formatTime(side.mtime)}</span>
                </div>
              {:else}
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <div class="side filler" data-side={i === 0 ? "left" : "right"} onmousedown={() => (activeSide = i === 0 ? "left" : "right")} oncontextmenu={(e) => { e.preventDefault(); rowMenu(row.path, i === 0 ? "left" : "right", e.clientX, e.clientY); }}></div>
              {/if}
            {/each}
          </div>
        {/each}
      </div>
    </div>
  </div>

  <div class="status">
    {#if running}
      <span>Comparing… <b>{progress.toLocaleString()}</b> files</span>
      <button class="small" onclick={() => api.cancelCompare(session.id)}>Cancel</button>
    {:else if result && counts}
      {@const s = result.summary}
      <span>Showing <b>{matchingFiles.toLocaleString()}</b> of <b>{counts.files.toLocaleString()}</b> files</span>
      <span>compared in <b>{s.elapsedMs < 1000 ? `${s.elapsedMs} ms` : `${(s.elapsedMs / 1000).toFixed(2)} s`}</b>{s.identicalSubtrees ? ` · ${s.identicalSubtrees.toLocaleString()} identical folders skipped via Git tree ids` : ""}</span>
    {/if}
    {#if busy}<span>{busy}</span>{/if}
    {#if op}
      <span>{op.label}…{op.total ? ` ${op.done.toLocaleString()} / ${op.total.toLocaleString()}` : ""}</span>
      <button class="small" onclick={() => api.cancelOp(session.id)}>Cancel</button>
    {/if}
    {#if targets.length > 1}<span>{targets.length} selected</span>{/if}
    {#if error}<span class="c-err" title={error}>{error}</span>{/if}
  </div>
  {#if menu}
    <Menu items={menu.items} context={menu.context} x={menu.x} y={menu.y} returnFocus={viewport} onClose={() => (menu = null)} />
  {/if}
</div>

<style>
  .view {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
    padding: 6px 8px;
    background: var(--panel);
    border-bottom: 1px solid var(--line);
  }
  .filter-scope { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; padding: 4px 10px; background: var(--panel-2); color: var(--dim); font-size: 11px; border-bottom: 1px solid var(--line); }
  .filter-scope strong { color: var(--accent); font-weight: 600; }
  .filter-scope button { font-size: 11px; padding: 0 6px; }
  .check {
    display: flex;
    gap: 4px;
    align-items: center;
    margin: 0 4px;
  }
  .grow {
    flex: 1;
  }
  .patterns {
    position: relative;
    display: inline-flex;
    align-items: center;
  }
  .patterns input {
    width: 150px;
    padding: 4px 22px 4px 7px;
    font-size: 12.5px;
  }
  .patterns.active input {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent);
  }
  .patterns .clear {
    position: absolute;
    right: 3px;
    border: none;
    background: none;
    padding: 0 4px;
    color: var(--dim);
  }
  .pathbars,
  .head {
    display: grid;
    grid-template-columns: 1fr 28px 1fr;
  }
  .pathbars {
    grid-template-columns: 1fr 1fr;
    gap: 1px;
    background: var(--line);
    border-bottom: 1px solid var(--line);
  }
  .head {
    background: var(--panel);
    color: var(--dim);
    border-bottom: 1px solid var(--line);
    height: 24px;
    align-items: center;
  }
  .side {
    display: grid;
    grid-template-columns: 1fr 78px 124px;
    gap: 6px;
    padding: 0 8px;
    min-width: 0;
    align-items: center;
    height: 100%;
  }
  .num {
    text-align: right;
    color: var(--dim);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .viewport {
    flex: 1;
    overflow: auto;
    position: relative;
    outline: none;
  }
  .spacer {
    position: relative;
  }
  .window {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
  }
  .row {
    display: grid;
    grid-template-columns: 1fr 28px 1fr;
    height: 22px;
    cursor: default;
    white-space: nowrap;
  }
  .row.sel {
    background: var(--sel);
  }
  .viewport:focus .row.focus {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
  }
  .row .mid,
  .head .mid {
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--panel-2);
    border-left: 1px solid var(--line);
    border-right: 1px solid var(--line);
    color: var(--dim);
    height: 100%;
    font-size: 11px;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .dir {
    color: var(--dim);
  }
  /* Ignored by Git: shown and compared, but set back. */
  .side.ignored .name {
    font-style: italic;
    opacity: 0.75;
  }
  .ignored-tag {
    margin-left: 6px;
    padding: 0 5px;
    border: 1px solid var(--line);
    border-radius: 8px;
    font-size: 10.5px;
    font-style: normal;
    color: var(--dim);
  }
  .chev {
    display: inline-block;
    width: 14px;
    color: var(--dim);
    cursor: pointer;
  }
  .icon {
    display: inline-block;
    width: 20px;
    font-size: 12px;
  }
  .filler {
    background: var(--filler);
  }
  .different .name,
  .different .mid {
    color: var(--diff);
  }
  .similar .name,
  .similar .mid {
    color: var(--similar);
  }
  .leftOnly .name,
  .leftOnly .mid {
    color: var(--left-only);
  }
  .rightOnly .name,
  .rightOnly .mid {
    color: var(--right-only);
  }
  .error .name,
  .error .mid {
    color: var(--error);
    font-style: italic;
  }
  .empty {
    position: absolute;
    inset: 40px 0 auto;
    text-align: center;
    color: var(--dim);
  }
  .status {
    display: flex;
    flex-wrap: wrap;
    gap: 16px;
    align-items: center;
    padding: 4px 10px;
    min-height: 26px;
    background: var(--chrome);
    border-top: 1px solid var(--line);
    color: var(--dim);
    font-size: 12px;
  }
  .status b {
    color: var(--text);
    font-weight: 600;
  }
  .small {
    padding: 0 8px;
  }
  .c-err {
    color: var(--error);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 60ch;
  }
</style>
