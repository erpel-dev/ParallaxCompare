<script lang="ts">
  import { summarizeErrors, exportErrors } from "./errorSummary";
  import { selectionTargets } from "./selection";
  import { splitPatterns, joinPatterns } from "./patterns";
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { ask } from "./confirm.svelte";
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
    closedArchive,
    flatFiles,
    flatten,
    joinPath,
    mergePartial,
    movedPairs,
    pairRows,
    refreshTarget,
    replaceNode,
    type Counts,
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
  /** Files found so far in the folders listed, and when that number last grew. */
  let listed = $state(0);
  let listedAt = 0;
  /** The view's width: narrow windows get a one-row header. */
  let width = $state(1400);
  const compact = $derived(width < 1000);
  const narrow = $derived(width < 760);
  let errorsDismissed = $state(false);
  let error = $state("");
  let patternError = $state("");
  let excludesError = $state("");
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
    if (!result) return null;
    const totals = flat ? countFlatFiles(flatRows) : countFiles(result.root);
    // The root is not a row, but its own failure belongs in the error totals.
    if (result.root.status === "error") {
      totals.files++;
      totals.errors++;
    }
    return totals;
  });
  const countFilters = $derived<CountFilter[]>([
    { id: "all", label: "All", count: counts?.files ?? 0, title: "Show all files" },
    { id: "diffs", label: "All differences", count: counts ? matchingCount(counts, "diffs") : 0, title: "Changed, left-only, right-only, and error entries", tone: "different" },
    { id: "same", label: "Same", count: counts?.same ?? 0, tone: "same" },
    { id: "similar", label: "Similar", count: counts?.similar ?? 0, title: "Equal content, different bytes", tone: "similar" },
    { id: "different", label: "Different", count: counts?.different ?? 0, title: "Changed files present on both sides", tone: "different" },
    { id: "leftOnly", label: "Left only", count: counts?.leftOnly ?? 0, tone: "left" },
    { id: "rightOnly", label: "Right only", count: counts?.rightOnly ?? 0, tone: "right" },
    { id: "error", label: "Errors", count: counts?.errors ?? 0, tone: "error" },
  ]);
  const matchingFiles = $derived(counts ? matchingCount(counts, filter) : 0);

  /** What a folder holds by status, for the counts on collapsed folders. */
  const insideMemo = $derived.by(() => {
    void version;
    void result;
    return new WeakMap<Node, Counts>();
  });
  function inside(node: Node): Counts {
    let c = insideMemo.get(node);
    if (!c) insideMemo.set(node, (c = countFiles(node)));
    return c;
  }

  /** The share of each status among the files, for the bar under the filters. */
  const shares = $derived.by(() => {
    if (!counts || !counts.files) return [];
    const parts: [Filter, string, number, string][] = [
      ["same", "same", counts.same, "Same"],
      ["similar", "similar", counts.similar, "Similar"],
      ["different", "different", counts.different, "Different"],
      ["leftOnly", "left", counts.leftOnly, "Left only"],
      ["rightOnly", "right", counts.rightOnly, "Right only"],
      ["error", "error", counts.errors, "Errors"],
    ];
    return parts.filter((p) => p[2] > 0).map(([id, tone, n, label]) => ({ id, tone, n, label, share: n / counts.files }));
  });

  /** Summarize only settled trees; progress events can update hundreds of times. */
  const errorInfo = $derived.by(() => {
    if (running || !result) return null;
    void version;
    return summarizeErrors(result.root);
  });

  async function copyErrors() {
    if (!result) return;
    const list = exportErrors(result.root);
    try {
      await navigator.clipboard.writeText(list.text);
      if (list.truncated) notify(`Copied ${list.copied.toLocaleString()} of ${list.total.toLocaleString()} errors. The list reached its export limit.`);
    } catch (e) {
      notify(`Could not copy the error list: ${String(e)}`);
    }
  }

  /** What is left of a running comparison, once the number of files has settled. */
  const remaining = $derived.by(() => {
    const total = Math.max(listed, progress);
    if (!running || !total) return null;
    const settled = performance.now() - listedAt > 1500 && elapsed > 1500;
    const left = settled && progress > 20 && progress < total ? Math.round(((elapsed / progress) * (total - progress)) / 1000) : null;
    return { total, share: Math.min(1, progress / total), left };
  });

  /** "+2.9 KB": how much larger or smaller the right file is. */
  function changeOf(node: Node): string {
    const { left, right } = node;
    if (!left || !right || left.kind === "dir" || right.kind === "dir" || (node.status !== "different" && node.status !== "similar")) return "";
    const delta = right.size - left.size;
    return delta === 0 ? "same size" : `${delta > 0 ? "+" : "−"}${formatSize(Math.abs(delta))}`;
  }

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
    catch (e) { patternError = String(e); return; }
    patternError = "";
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

  // ---- Progress of a comparison ----

  /** Slow work going on in the comparison, such as decompressing an archive. */
  let activity = $state<string | null>(null);
  let startedAt = 0;
  let elapsed = $state(0);
  let ticker: ReturnType<typeof setInterval> | undefined;
  /** How the last comparison ended when it was cancelled: shown until the next one. */
  let cancelled = $state<{ seconds: number } | null>(null);
  let cancelling = $state(false);

  function cancelRun() {
    cancelling = true;
    api.cancelCompare(session.id);
  }

  const seconds = (ms: number) => (ms < 10_000 ? (ms / 1000).toFixed(1) : Math.round(ms / 1000).toString());

  /**
   * Compares again. A refresh (`invalidate`) keeps showing the old result until the
   * new one is there; otherwise the entries show as the compare finds them.
   */
  async function run(invalidate = false) {
    running = true;
    error = "";
    progress = 0;
    activity = null;
    cancelled = null;
    cancelling = false;
    startedAt = performance.now();
    elapsed = 0;
    listed = 0;
    listedAt = startedAt;
    errorsDismissed = false;
    clearInterval(ticker);
    ticker = setInterval(() => (elapsed = performance.now() - startedAt), 200);
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
      const wasCancelled = String(e) === "cancelled";
      if (wasCancelled && cancelling) {
        // What was found so far stays on screen, said to be partial.
        cancelled = { seconds: (performance.now() - startedAt) / 1000 };
        provisional = false;
        version++;
      } else if (!wasCancelled) {
        error = String(e);
      }
      if (provisional && !cancelled) {
        provisional = false;
        result = null;
        version++;
      }
    } finally {
      running = false;
      cancelling = false;
      activity = null;
      clearInterval(ticker);
    }
    // Changes that came in during the compare are in the new result already.
    deferred.clear();
    deferredFull = false;
  }

  onMount(() => {
    run();
    const unlisten = [
      listen<{ session: number; files: number; listed: number; partial?: Partial; activity?: string }>("compare-progress", (e) => {
        if (e.payload.session !== session.id) return;
        progress = e.payload.files;
        if (e.payload.listed !== listed) {
          listed = e.payload.listed;
          listedAt = performance.now();
        }
        activity = e.payload.activity ?? null;
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
    catch (e) { excludesError = String(e); return; }
    excludesError = "";
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

  /** Files that differ (or exist on one side only, or failed), in tree order, also in closed folders. */
  function differingFiles(): string[] {
    if (!result) return [];
    const out: string[] = [];
    const walk = (node: Node, dir: string) => {
      for (const child of node.children ?? []) {
        const path = dir ? `${dir}/${child.name}` : child.name;
        const container = (child.children?.length ?? 0) > 0;
        if (container) walk(child, path);
        else if (child.status !== "same" && child.status !== "pending" && filterAccepts(child)) out.push(path);
      }
    };
    walk(result.root, "");
    return out;
  }

  const filterAccepts = (node: Node) => filter === "all" || filter === "diffs" || selfMatches(node, filter);

  /** Selects the next (or previous) file that differs, opening folders on the way. */
  function nextDifference(dir: 1 | -1) {
    const files = differingFiles();
    if (!files.length) return;
    const at = selected ? files.findIndex((p) => p === selected) : -1;
    let index: number;
    if (at >= 0) index = (at + dir + files.length) % files.length;
    else {
      // From a row that is not a differing file: the next one after it in tree order.
      const after = selected ? files.findIndex((p) => p.localeCompare(selected!) > 0) : 0;
      index = dir > 0 ? (after < 0 ? 0 : after) : (after <= 0 ? files.length - 1 : after - 1);
    }
    revealPath(files[index]);
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "F7" || e.key === "F8") {
      nextDifference(e.key === "F8" ? 1 : -1);
      e.preventDefault();
      return;
    }
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
      case "Escape":
        if (!selection.size) return;
        selection = new Set();
        e.preventDefault();
        return;
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
    return selectionTargets(paths);
  });

  /** Shared by action availability and operations; indexed in both tree and flat views. */
  const presentTargets = $derived.by(() => {
    void version;
    const sides: [string[], string[]] = [[], []];
    for (const path of targets) {
      const node = rowByPath.get(path)?.node;
      if (node?.left) sides[0].push(path);
      if (node?.right) sides[1].push(path);
    }
    return sides;
  });
  const present = (left: boolean): string[] => presentTargets[left ? 0 : 1];

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
    const paired = new Set(pairs.map((r) => r.path));
    const paths = all.filter((p) => !paired.has(p));
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
      for (const f of copyFilesBelow(rowByPath.get(p)!.node, p, toRight)) {
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

{#snippet legend()}
  <div class="lrow"><span class="g">=</span><b>Same</b><span>Byte for byte{options.mode === "quick" ? ", by size and date only" : ", or the same checksum in an archive or object in Git"}</span></div>
  <div class="lrow"><span class="g sim">≈</span><b>Similar</b><span>The bytes differ, but the content is equal under a rule that is on (see below). The detail line under the tree says which.</span></div>
  <div class="lrow"><span class="g diff">≠</span><b>Different</b><span>The content differs</span></div>
  <div class="lrow"><span class="g left">◀</span><b>Left only</b><span>Only on the left side</span></div>
  <div class="lrow"><span class="g right">▶</span><b>Right only</b><span>Only on the right side</span></div>
  <div class="lrow"><span class="g diff">!</span><b>Error</b><span>Could not be read or compared</span></div>
  <div class="foot">
    Rules for ≈, set in Options:
    <span class:on={options.ignoreLineEndings}>line endings {options.ignoreLineEndings ? "ignored" : "count"}</span> ·
    <span class:on={options.compareContents}>equal content {options.compareContents ? "is similar" : "is different"}</span> (documents, images, data such as JSON, YAML, TOML, code without comments)
  </div>
{/snippet}

{#snippet treeButtons()}
  <button onclick={() => setAll(true)} disabled={flat} title="Expand all folders">Expand</button>
  <button onclick={() => setAll(false)} disabled={flat} title="Collapse all folders">Collapse</button>
  <button class:on={flat} onclick={() => setFlat(!flat)} title="All files in one list, without folders; files of the same name are matched across folders">Flat</button>
{/snippet}

<div class="view" class:compact class:narrow bind:clientWidth={width}>
  <div class="toolbar">
    {#if nav}<Nav {nav} /><span class="sep"></span>{/if}
    <button onclick={() => run(true)} disabled={running} title="Compare again (F5)">⟳{#if !compact}{" "}Refresh{/if}</button>
    <button onclick={onSwap} title="Swap left and right">⇄{#if !compact}{" "}Swap{/if}</button>
    <span class="sep"></span>
    <button onclick={() => nextDifference(-1)} disabled={!counts || matchingCount(counts, "diffs") === 0} title="Previous difference (F7)">‹{#if !compact}{" "}Previous{/if}</button>
    <button onclick={() => nextDifference(1)} disabled={!counts || matchingCount(counts, "diffs") === 0} title="Next difference (F8)">{#if !compact}Next difference{" "}{/if}›</button>
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
    {#if compact}
      <FilterBar items={countFilters} value={filter} onSelect={(id) => setFilter(id as Filter)} disabled={!result} {legend} compact inline />
    {:else}
      <span class="sep"></span>
      {@render treeButtons()}
    {/if}
    <span class="grow"></span>
    <Dropdown label={compact ? "⋯" : "Options"} title={compact ? "Expand, collapse, flat list and compare options" : "Compare rule, line endings, excluded files"} changed={optionsChanged}>
      {#if compact}<div class="treebtns">{@render treeButtons()}</div>{/if}
      <p class="scope">Applies to this comparison and the files opened from it.</p>
      <div class="field">
        <span>Compare by</span>
        <select bind:value={options.mode} onchange={() => run()}>
          <option value="content">Content</option>
          <option value="quick">Size &amp; date (faster)</option>
        </select>
      </div>
      {#if options.mode === "quick"}<p class="scope">Size &amp; date compares no contents: the options below need Compare by: Content.</p>{/if}
      <label title="Files that differ only in CRLF or LF line breaks are marked ≈ instead of ≠">
        <input type="checkbox" bind:checked={options.ignoreLineEndings} disabled={options.mode === "quick"} onchange={() => run()} /> Ignore CRLF/LF differences <small>affects ≈</small>
      </label>
      <label title="Documents with the same text, images with the same pixels, and data files (JSON, YAML, TOML …) with the same data are marked ≈ instead of ≠">
        <input type="checkbox" bind:checked={options.compareContents} disabled={options.mode === "quick"} onchange={() => run()} /> Equal content counts as similar <small>affects ≈</small>
      </label>
      <label title="Zip and 7z entries with the same CRC32 and size are taken as equal without decompressing them. Different data shares a CRC32 only by a one-in-four-billion chance or when made to. Verifying decompresses every such entry, which takes seconds for large solid 7z archives.">
        <input type="checkbox" bind:checked={options.verifyChecksums} disabled={options.mode === "quick"} onchange={() => run()} /> Verify archive entries byte for byte <small>affects =</small>
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

  {#if running}
    <div class="progress" role="status">
      <div class="pline">
        <b>{cancelling ? "Cancelling…" : (activity ?? "Comparing")}</b>
        {#if remaining}
          <span><b>{progress.toLocaleString()}</b> of {remaining.total.toLocaleString()} files</span>
        {:else}
          <span>Listing…</span>
        {/if}
        <span>{seconds(elapsed)} s{#if remaining?.left != null}{" "}· about {remaining.left < 1 ? "1" : remaining.left.toLocaleString()} s left{/if}</span>
        <span class="grow"></span>
        <button class="small" onclick={cancelRun} disabled={cancelling}>Cancel</button>
      </div>
      <div class="track" aria-hidden="true">{#if remaining}<i class="det" style:width="{remaining.share * 100}%"></i>{:else}<i></i>{/if}</div>
    </div>
  {:else if cancelled}
    <div class="progress done" role="status">
      <div class="pline">
        <b class="warn">Cancelled after {cancelled.seconds.toFixed(1)} s</b>
        <span>The list is partial: it holds what was finished before the stop.</span>
        <span class="grow"></span>
        <button class="small primary" onclick={() => run()}>Compare again</button>
      </div>
    </div>
  {/if}

  {#if errorInfo && !errorsDismissed && (filter !== "error" || result?.root.status === "error")}
    <div class="banner" role="alert">
      <b>{errorInfo.count.toLocaleString()} {errorInfo.count === 1 ? "entry" : "entries"} could not be read or compared</b>
      <span>{errorInfo.top.map(([cause, n]) => `${cause} (${n.toLocaleString()})`).join(", ")}{errorInfo.more > 0 ? `, ${errorInfo.more} other causes` : ""}</span>
      <span class="grow"></span>
      {#if result?.root.status !== "error" || errorInfo.count > 1}<button class="link" onclick={() => setFilter("error")}>Show them</button>{/if}
      <button class="link" onclick={() => run(true)} title="Compare again, reading archives and folders afresh">Retry</button>
      <button class="link" onclick={copyErrors} title="Copy paths and messages (up to 20,000 entries or 4 MiB; truncation is reported)">Copy the list</button>
      <button class="link x" onclick={() => (errorsDismissed = true)} title="Hide this note; the Errors filter stays" aria-label="Hide">✕</button>
    </div>
  {/if}
  {#if error || patternError || excludesError}
    <div class="banner" role="alert">
      <b>{error ? "That did not work" : "Pattern not valid"}</b>
      <span class="msg">{error || patternError || excludesError}</span>
      <span class="grow"></span>
      {#if error}<button class="link x" onclick={() => (error = "")} aria-label="Hide">✕</button>{/if}
    </div>
  {/if}

  {#if !compact}
    <FilterBar items={countFilters} value={filter} onSelect={(id) => setFilter(id as Filter)} disabled={!result} {legend}
      hint={running ? "Totals update during comparison" : flatRows.some((r) => r.pending) ? "Matched files are still being compared" : ""} />
  {/if}
  {#if shares.length > 1}
    <div class="shares" role="group" aria-label="Share of each status; click to filter">
      {#each shares as part (part.id)}
        <button class={part.tone} class:on={filter === part.id} style:flex-grow={Math.max(part.share, 0.004)}
          title="{part.label}: {part.n.toLocaleString()} of {counts!.files.toLocaleString()} files ({(part.share * 100).toFixed(part.share < 0.1 ? 1 : 0)} %). Click to show only these."
          aria-label="{part.label}: {part.n}" onclick={() => setFilter(filter === part.id ? "all" : part.id)}></button>
      {/each}
    </div>
  {/if}

  <div class="pathbars">
    <PathBar bind:this={pathbars[0]} info={session.left} edit={edit?.[0]} title="Left" active={activeSide === "left"} />
    <PathBar bind:this={pathbars[1]} info={session.right} edit={edit?.[1]} title="Right" active={activeSide === "right"} />
  </div>

  <div class="head">
    <div class="side"><span>Name</span><span class="num">Size</span><span class="num mod">Modified</span></div>
    <div class="mid"></div>
    <div class="side r"><span>Name</span><span class="num">Size</span><span class="num chg" title="How the right side differs in size from the left">Change</span><span class="num mod">Modified</span></div>
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
      <div class="empty">{result.root.status === "error" ? (result.root.message ?? "The compared root could not be read. Retry the comparison.") : filter !== "all" ? "Nothing matches the current filter." : result.root.status === "same" ? "Both sides are identical." : "No files to compare."}</div>
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
                {@const other = i === 0 ? row.node.right : row.node.left}
                <!-- svelte-ignore a11y_no_static_element_interactions -->
                <div class="side" class:r={i === 1} class:ignored={side.ignored} data-side={i === 0 ? "left" : "right"} onmousedown={() => (activeSide = i === 0 ? "left" : "right")} oncontextmenu={(e) => { e.preventDefault(); rowMenu(row.path, i === 0 ? "left" : "right", e.clientX, e.clientY); }}>
                  <span class="name" style:padding-left="{row.depth * 16}px">
                    {#if row.container}
                      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                      <span class="chev" onclick={(e) => { e.stopPropagation(); toggle(row.path); }}>{row.expanded ? "▾" : "▸"}</span>
                    {:else}
                      <span class="chev"></span>
                    {/if}
                    <span class="icon">{icon(side)}</span>{#if row.folders?.[i]}<span class="dir">{row.folders[i]}/</span>{/if}{i === 1 && row.rightPath ? row.rightPath.split("/").pop() : row.node.name}{#if side.ignored && !row.inIgnored?.[i]}<span class="ignored-tag" title="Ignored by Git: a .gitignore rule matches it, and it is not tracked">ignored</span>{/if}{#if row.container && !row.expanded && !row.pending && !closedArchive(row.node) && (i === 0 || !row.node.left)}{@const c = inside(row.node)}{#if c.files > c.same}<span class="inside" title="Files in this folder that are not the same, by status">{#if c.different}<span class="different">{c.different.toLocaleString()} ≠</span>{/if}{#if c.similar}<span class="similar">{c.similar.toLocaleString()} ≈</span>{/if}{#if c.leftOnly}<span class="leftOnly">{c.leftOnly.toLocaleString()} ◀</span>{/if}{#if c.rightOnly}<span class="rightOnly">{c.rightOnly.toLocaleString()} ▶</span>{/if}{#if c.errors}<span class="error">{c.errors.toLocaleString()} !</span>{/if}</span>{/if}{/if}
                  </span>
                  <span class="num">{side.kind === "dir" ? "" : formatSize(side.size)}</span>
                  {#if i === 1}<span class="num chg">{changeOf(row.node)}</span>{/if}
                  <span class="num mod" class:newer={!!other && !!side.mtime && !!other.mtime && side.mtime > other.mtime && row.node.status !== "same" && side.kind !== "dir"} title={!!other && !!side.mtime && !!other.mtime && side.mtime > other.mtime && side.kind !== "dir" ? "Newer than the other side" : undefined}>{formatTime(side.mtime)}</span>
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

  {#if selection.size > 0 && result}
    <div class="selbar" role="toolbar" aria-label="Selected entries">
      <b>{selection.size === 1 ? "1 selected" : `${selection.size.toLocaleString()} selected`}</b>
      <button disabled={!canCopy[1] || !!op} onclick={() => copy(true)} title="Copy the selection from the left side to the right side">Copy left → right</button>
      <button disabled={!canCopy[0] || !!op} onclick={() => copy(false)} title="Copy the selection from the right side to the left side">Copy right → left</button>
      {#if pair && onCompareRows}<button onclick={() => onCompareRows?.(pair![0], pair![1])} title="Compare the two selected entries with each other, e.g. a file renamed between the sides">⇆ Compare these</button>{/if}
      <span class="grow"></span>
      <button class="danger" disabled={!canDelete[0] || !!op} onclick={() => remove(true)}>Delete on the left…</button>
      <button class="danger" disabled={!canDelete[1] || !!op} onclick={() => remove(false)}>Delete on the right…</button>
      <button class="link" onclick={() => { selection = new Set(); selected = null; }} title="Clear the selection (Esc)">Clear</button>
    </div>
  {/if}

  {#if selectedRow && result}
    {@const n = selectedRow.node}
    {@const label = { same: "Same", similar: "Similar", different: "Different", leftOnly: "Left only", rightOnly: "Right only", error: "Error", pending: "Being compared" }[n.status]}
    <div class="detailline" aria-live="polite">
      <b class="name">{n.name}</b>
      <span class="st {n.status}">{glyph[n.status]} {label}</span>
      {#if n.message}<span>{n.message}</span>{/if}
      {#if n.status === "similar"}
        <span class="dim">rule: {n.message === "only line endings differ" ? "Ignore CRLF/LF differences" : "Equal content counts as similar"}</span>
      {:else if n.status === "same" && !selectedRow.container}
        <span class="dim">{options.mode === "quick" ? "same size and date" : "same bytes"}</span>
      {/if}
      <span class="grow"></span>
      {#if !selectedRow.container && n.left && n.right}<button class="link" onclick={() => onOpenFile(selectedRow!.path)}>Open comparison (Enter)</button>{/if}
    </div>
  {/if}

  <div class="status">
    {#if running}
      <span>Comparing…</span>
    {:else if result && counts}
      {@const s = result.summary}
      <span>Showing <b>{matchingFiles.toLocaleString()}</b> of <b>{counts.files.toLocaleString()}</b> files</span>
      {#if cancelled}<span>partial: cancelled</span>{:else}<span>compared in <b>{s.elapsedMs < 1000 ? `${s.elapsedMs} ms` : `${(s.elapsedMs / 1000).toFixed(2)} s`}</b>{s.identicalSubtrees ? ` · ${s.identicalSubtrees.toLocaleString()} identical folders skipped via Git tree ids` : ""}</span>{/if}
    {/if}
    {#if busy}<span>{busy}</span>{/if}
    {#if op}
      <span>{op.label}…{op.total ? ` ${op.done.toLocaleString()} / ${op.total.toLocaleString()}` : ""}</span>
      <button class="small" onclick={() => api.cancelOp(session.id)}>Cancel</button>
    {/if}
    {#if targets.length > 1}<span>{targets.length} selected</span>{/if}
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
  .progress { display: grid; gap: 4px; padding: 5px 10px; border-bottom: 1px solid var(--line); background: var(--panel-2); font-size: 12px; }
  .progress .pline { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; color: var(--dim); }
  .progress .pline b { color: var(--fg); font-weight: 600; }
  .progress .pline b.warn { color: var(--similar); }
  .progress .grow { flex: 1; }
  .track { height: 3px; border-radius: 2px; background: var(--line); overflow: hidden; position: relative; }
  .track i { position: absolute; top: 0; bottom: 0; width: 30%; background: var(--accent); border-radius: 2px; animation: sweep 1.4s ease-in-out infinite; }
  @keyframes sweep { from { left: -30%; } to { left: 100%; } }
  @media (prefers-reduced-motion: reduce) { .track i { animation: none; left: 35%; opacity: 0.6; } }
  .track i.det { position: static; display: block; height: 100%; animation: none; transition: width 0.2s linear; }
  .banner { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 12px; padding: 5px 10px; border-bottom: 1px solid var(--line); background: color-mix(in srgb, var(--error) 13%, var(--panel-2)); font-size: 12px; }
  .banner b { color: var(--error); font-weight: 600; }
  .banner .msg { overflow-wrap: anywhere; min-width: 0; }
  .banner .grow { flex: 1; }
  .banner .link { border: none; background: none; color: var(--accent); padding: 0 4px; cursor: pointer; }
  .banner .x { color: var(--dim); }
  .shares { display: flex; gap: 1px; height: 7px; background: var(--panel-2); border-bottom: 1px solid var(--line); }
  .shares button { flex: 1 1 0; min-width: 3px; padding: 0; border: none; border-radius: 0; background: var(--faint); opacity: 0.85; cursor: pointer; }
  .shares button:hover, .shares button.on { opacity: 1; box-shadow: inset 0 -2px var(--fg); }
  .shares .similar { background: var(--similar); }
  .shares .different { background: var(--diff); }
  .shares .left { background: var(--left-only); }
  .shares .right { background: var(--right-only); }
  .shares .error { background: var(--error); }
  .inside { display: inline-flex; gap: 8px; margin-left: 10px; font-family: var(--mono); font-size: 10.5px; }
  .inside .different, .inside .error { color: var(--diff); }
  .inside .similar { color: var(--similar); }
  .inside .leftOnly { color: var(--left-only); }
  .inside .rightOnly { color: var(--right-only); }
  .treebtns { display: flex; gap: 4px; padding-bottom: 8px; border-bottom: 1px solid var(--line); }
  .side.r { grid-template-columns: 1fr 78px 72px 124px; }
  .chg { font-size: 11px; }
  .mod.newer { color: var(--fg); font-weight: 600; }
  /* Narrow windows: names before dates, then before the change column. */
  .view.compact .side { grid-template-columns: 1fr 70px; }
  .view.compact .side.r { grid-template-columns: 1fr 70px 68px; }
  .view.compact .mod { display: none; }
  .view.narrow .side.r { grid-template-columns: 1fr 64px; }
  .view.narrow .chg { display: none; }
  .view.compact .patterns { flex: 1 1 110px; min-width: 80px; }
  .view.compact .toolbar { flex-wrap: nowrap; }
  .selbar { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; padding: 4px 10px; border-top: 1px solid var(--line); background: color-mix(in srgb, var(--accent) 9%, var(--panel-2)); font-size: 12px; }
  .selbar b { margin-right: 6px; }
  .selbar button { font-size: 12px; padding: 2px 8px; }
  .selbar .grow { flex: 1; }
  .selbar .danger { color: var(--error); }
  .selbar .link, .detailline .link { border: none; background: none; color: var(--accent); padding: 0 4px; cursor: pointer; }
  .detailline { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 14px; padding: 4px 10px; border-top: 1px solid var(--line); background: var(--panel-2); font-size: 12px; min-height: 24px; }
  .detailline .name { font-weight: 600; }
  .detailline .st { font-weight: 600; }
  .detailline .st.similar { color: var(--similar); }
  .detailline .st.different, .detailline .st.error { color: var(--diff); }
  .detailline .st.leftOnly { color: var(--left-only); }
  .detailline .st.rightOnly { color: var(--right-only); }
  .detailline .dim { color: var(--dim); }
  .detailline .grow { flex: 1; }
  .scope { margin: 0; font-size: 11.5px; color: var(--dim); max-width: 300px; }
  label small { color: var(--dim); font-size: 10.5px; margin-left: auto; padding-left: 8px; white-space: nowrap; }
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
</style>
