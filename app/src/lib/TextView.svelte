<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { ask, open } from "@tauri-apps/plugin-dialog";
  import {
    api,
    defaultDiffOptions,
    sourceText,
    END,
    theme,
    type EditInfo,
    type FileDiffInfo,
    type Hit,
    type LineView,
    type PathsChanged,
    type RowView,
    type SearchOptions,
    type SessionInfo,
    type SideName,
    type View,
  } from "./api";
  import ImagePanel from "./ImagePanel.svelte";
  import FilterBar, { type CountFilter } from "./FilterBar.svelte";
  import { RowProjection, type RowFilter } from "./rowFilter";
  import Menu, { type MenuItem, type MenuContext } from "./Menu.svelte";
  import type { Command } from "./commands";
  import Dropdown from "./Dropdown.svelte";
  import Nav, { type NavActions } from "./Nav.svelte";
  import PathBar, { type SideEdit } from "./PathBar.svelte";
  import { scaledWheel, scrollScale, spacerHeight, windowOffset } from "./vscroll";
  import { formatSize } from "./format";
  import type { TextViewState } from "./viewState";
  import VersionStrip from "./VersionStrip.svelte";
  import { loadVersions } from "./versions";

  let {
    session,
    path,
    onDirty,
    onSwap,
    edit,
    nav,
    restore,
  }: {
    session: SessionInfo;
    path: string;
    onDirty?: (dirty: boolean) => void;
    onSwap?: () => void;
    edit?: [SideEdit, SideEdit];
    nav?: NavActions;
    /** How the comparison this one replaced was shown: scroll position, selected row, view. */
    restore?: TextViewState;
  } = $props();

  const ROW = 19;
  const CHUNK = 200;
  /** Loaded chunks kept; those farthest from the view are dropped beyond this. */
  const KEEP_CHUNKS = 60;
  /**
   * Requests at a time. The backend answers them one after another, so more would
   * only queue up rows the view has scrolled past by the time they come back.
   */
  const PARALLEL = 2;
  const SIDE_NAMES: [SideName, SideName] = ["left", "right"];

  let options = $state(defaultDiffOptions());
  let syntaxOverride = $state<string>("");
  let view = $state<View | null>(null);
  let flatbufferSchemas = $state<[string | null, string | null]>([null, null]);
  let syntaxNames = $state<string[]>([]);
  // Raw: a diff can have hundreds of thousands of sections, and deep reactivity
  // would wrap every one of them. `info` is only ever replaced as a whole.
  let info = $state.raw<FileDiffInfo | null>(null);
  let error = $state("");
  let loading = $state(true);
  let busy = $state("");

  // Rows are fetched lazily in chunks; the counter re-renders when chunks arrive.
  let cache = new Map<number, RowView>();
  let pending = new Set<number>();
  let loaded = new Set<number>();
  let inFlight = 0;
  let cacheVersion = $state(0);
  /** Changes when the rows are replaced (new diff, edit), so older answers are dropped. */
  let generation = 0;

  let viewport = $state<HTMLDivElement>();
  let scrollTop = $state(0);
  let viewHeight = $state(600);
  let hscroll = $state(0);
  let selected = $state<number | null>(null);
  /** Where a line selection started (Shift+click, Shift+↑/↓); null for the current row alone. */
  let anchor = $state<number | null>(null);
  let section = $state(-1);

  // Editing state.
  let dirty = $state<[boolean, boolean]>([false, false]);
  let canUndo = $state(false);
  let canRedo = $state(false);
  /** The side that keyboard commands (Enter, copy) act on: the last one clicked. */
  let activeSide = $state<0 | 1>(1);
  let menu = $state<{ x: number; y: number; items: MenuItem[]; context: MenuContext } | null>(null);
  /** The line being typed in: its row, and its line number (null in a filler row). */
  let editing = $state<{ side: 0 | 1; row: number; line: number | null } | null>(null);
  let draft = $state("");
  let original = $state("");
  let input = $state<HTMLInputElement>();
  /** The editor opened last has its input focused, with the caret set. */
  let inputReady = false;
  /** After an edit, the caret waits for its row to be loaded. */
  let caretWanted: { side: 0 | 1; line: number; col: number; hint: number } | null = null;
  /** File changed on disk while it has unsaved edits. */
  let changedOnDisk = $state(false);
  // Edits run one after another, in the order they were made.
  let queue: Promise<unknown> = Promise.resolve();
  let applied: Promise<void> = Promise.resolve();
  /** Edits sent and not answered yet. */
  let unanswered = $state(0);
  /** An edit that moves the caret is on its way; the editor waits for where it goes. */
  let awaiting = $state(false);
  /** Keys and pastes held back while the editor waits, typed once it is back (see `replay`). */
  let ahead = $state.raw<Ahead[]>([]);
  type Ahead = { key: string; shift: boolean; ctrl: boolean; meta: boolean } | { paste: string };
  /** Counts the loads, so that an answer overtaken by a newer load is closed unseen. */
  let loads = 0;
  let destroyed = false;

  // Find.
  let root = $state<HTMLDivElement>();
  let findOpen = $state(false);
  let findQuery = $state("");
  let findOptions = $state<SearchOptions>({ matchCase: false, wholeWord: false, regex: false });
  let findInput = $state<HTMLInputElement>();
  let findError = $state("");
  let hits = $state.raw<Hit[]>([]);
  let hitIndex = $state(-1);
  let truncated = $state(false);
  let searchRun = 0;
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  /** The current match should be scrolled into view sideways once its row is drawn. */
  let revealHitWanted = false;

  let rowFilter = $state<RowFilter>("all");
  const total = $derived(info?.rows ?? 0);
  const projection = $derived(new RowProjection(info?.rowRuns ?? [], rowFilter));
  const displayTotal = $derived(projection.length);
  const rowFilters = $derived<CountFilter[]>([
    { id: "all", label: "All rows", count: total },
    { id: "diffs", label: "All differences", count: (info?.stats.changed ?? 0) + (info?.stats.added ?? 0) + (info?.stats.removed ?? 0), tone: "different" },
    { id: "equal", label: "Same", count: total - (info?.stats.changed ?? 0) - (info?.stats.added ?? 0) - (info?.stats.removed ?? 0) - (info?.stats.minor ?? 0), tone: "same" },
    { id: "minor", label: "Ignored", count: info?.stats.minor ?? 0, title: "Rows differing only in ignored whitespace or case", tone: "similar" },
    { id: "changed", label: "Changed", count: info?.stats.changed ?? 0, tone: "different" },
    { id: "removed", label: "Removed (left only)", count: info?.stats.removed ?? 0, tone: "left" },
    { id: "added", label: "Added (right only)", count: info?.stats.added ?? 0, tone: "right" },
  ]);
  const projectedSections = $derived.by(() => {
    const out: { index: number; start: number; end: number; source: number }[] = [];
    for (const [index, s] of (info?.sections ?? []).entries()) {
      const start = projection.position(s.start), end = projection.position(s.end);
      if (end > start) out.push({ index, start, end, source: projection.source(start)! });
    }
    return out;
  });

  async function setRowFilter(id: RowFilter) {
    if (!info || loading || awaiting) return;
    const diff = info.id;
    if (editing && draft !== original) commit(draft, null);
    await queue;
    await applied;
    if (info?.id !== diff || destroyed) return;
    editing = null;
    menu = null;
    rowFilter = id;
    anchor = null;
    selected = projection.source(0);
    section = -1;
    scrollTop = 0;
    if (viewport) viewport.scrollTop = 0;
    ensureLoaded();
  }

  const sectionOrdinals = $derived(new Map(projectedSections.map((s, i) => [s.index, i])));

  /** Editing and range selections use full context, so hidden lines cannot be changed unseen. */
  function showAllRows() {
    if (rowFilter === "all") return;
    rowFilter = "all";
    tick().then(() => { if (selected !== null) reveal(selected); });
  }

  /** Matches by row and side, for drawing. */
  const hitsAt = $derived.by(() => {
    const map = new Map<number, Hit[]>();
    for (const h of hits) {
      const key = h.row * 2 + h.side;
      const list = map.get(key);
      if (list) list.push(h);
      else map.set(key, [h]);
    }
    return map;
  });
  const currentHit = $derived(hitIndex >= 0 ? hits[hitIndex] : undefined);
  /** Rows with matches for the overview, at most one mark per pixel or so. */
  const hitMarks = $derived.by(() => {
    if (!displayTotal) return [];
    const buckets = new Set<number>();
    for (const h of hits) if (projection.contains(h.row)) buckets.add(Math.floor((projection.position(h.row) / displayTotal) * 600));
    return [...buckets].map((b) => b / 600);
  });
  // Very long files scroll through a scaled spacer (see vscroll.ts); `vTop`
  // is the scroll position in row pixels.
  const scale = $derived(scrollScale(displayTotal * ROW, viewHeight));
  const vTop = $derived(scrollTop * scale);
  const first = $derived(Math.max(0, Math.floor(vTop / ROW) - 20));
  const last = $derived(Math.min(displayTotal, first + Math.ceil(viewHeight / ROW) + 40));
  const visible = $derived.by(() => {
    void cacheVersion;
    const out: { index: number; row: RowView | undefined }[] = [];
    for (let i = first; i < last; i++) { const index = projection.source(i)!; out.push({ index, row: cache.get(index) }); }
    return out;
  });
  const selectedRow = $derived.by(() => {
    void cacheVersion;
    return selected === null ? undefined : cache.get(selected);
  });
  const editable = $derived<[boolean, boolean]>([!!info?.left.editable, !!info?.right.editable]);
  /** Rows of the line selection on the active side, end exclusive. */
  const lineRange = $derived.by<[number, number] | null>(() => {
    if (selected === null) return null;
    const a = anchor ?? selected;
    return [Math.min(a, selected), Math.max(a, selected) + 1];
  });
  /** Unsaved work: changed sides, but also the line being typed, edits on their way and held-back keys. */
  const anyDirty = $derived(
    dirty[0] || dirty[1] || (editing !== null && draft !== original) || unanswered > 0 || ahead.length > 0,
  );
  /** Section index by its first row, for the copy arrows. */
  /**
   * The overview's marks: sections merged per thousandth of the file, so a file
   * with 100 000 differences draws at most 1 000 marks.
   */
  const sectionMarks = $derived.by(() => {
    const marks: { top: number; height: number }[] = [];
    if (!info || !displayTotal) return marks;
    const B = 1000;
    let from = -1;
    let to = -1;
    for (const s of projectedSections) {
      const a = Math.floor((s.start / displayTotal) * B);
      const b = Math.max(a + 1, Math.ceil((s.end / displayTotal) * B));
      if (a <= to) to = Math.max(to, b);
      else {
        if (from >= 0) marks.push({ top: from / B, height: (to - from) / B });
        from = a;
        to = b;
      }
    }
    if (from >= 0) marks.push({ top: from / B, height: (to - from) / B });
    return marks;
  });
  /**
   * The difference the counter shows: the one moved to while it is on screen,
   * else the first one on screen, so the count follows scrolling.
   */
  /** The sections above the view, and the one shown in it (-1 for none). */
  const viewSections = $derived.by(() => {
    if (!info) return { above: 0, shown: -1 };
    const top = Math.floor(vTop / ROW);
    const bottom = top + Math.max(1, Math.floor(viewHeight / ROW));
    const onScreen = (s: { start: number; end: number }) => s.end > top && s.start < bottom;
    let lo = 0, hi = projectedSections.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (projectedSections[mid].end <= top) lo = mid + 1;
      else hi = mid;
    }
    const ordinal = sectionOrdinals.get(section);
    const current = ordinal === undefined ? undefined : projectedSections[ordinal];
    if (current && onScreen(current)) return { above: lo, shown: current.index };
    const first = projectedSections[lo];
    return { above: lo, shown: first && onScreen(first) ? first.index : -1 };
  });
  const shownSection = $derived(viewSections.shown);
  const shownOrdinal = $derived(sectionOrdinals.get(shownSection) ?? -1);
  const sectionStarts = $derived(new Map(projectedSections.map((s) => [s.source, s.index])));
  /** The section the toolbar's copy buttons act on. */
  const currentSection = $derived.by(() => {
    if (!info) return -1;
    if (selected !== null) {
      const i = info.sections.findIndex((s) => s.start <= selected! && selected! < s.end);
      if (i >= 0) return i;
    }
    return sectionOrdinals.has(section) ? section : -1;
  });

  $effect(() => onDirty?.(anyDirty));

  async function pickFlatbufferSchema(side: 0 | 1 | null) {
    if (anyDirty || loading) return;
    try {
      const selected = await open({
        title: side === null ? "FlatBuffers schema for both files" : `FlatBuffers schema for the ${side === 0 ? "left" : "right"} file`,
        multiple: false,
        filters: [{ name: "FlatBuffers schema", extensions: ["fbs", "bfbs"] }],
      });
      if (typeof selected !== "string") return;
      if (side === null) flatbufferSchemas = [selected, selected];
      else flatbufferSchemas[side] = selected;
      view = "flatbufferData";
      await load();
    } catch (e) {
      error = String(e);
    }
  }

  /** What the view shows, to show the same after one side was switched. */
  export function viewState(): TextViewState {
    return { scrollTop: viewport?.scrollTop ?? 0, selected, view };
  }

  // svelte-ignore state_referenced_locally
  let restoring: TextViewState | null = restore ?? null;

  // ---- A file with uncommitted changes, compared with something else ----

  /** The side whose file on disk has uncommitted changes, while the other side is not a version of it. */
  let uncommitted = $state<{ side: 0 | 1; name: string } | null>(null);
  let uncommittedDismissed = false;

  $effect(() => {
    void session.left.display;
    void session.right.display;
    const sides = edit;
    if (!sides || uncommittedDismissed) return;
    let stale = false;
    (async () => {
      const [l, r] = await Promise.all(sides.map((e) => e.spec().then((s) => loadVersions(s)).catch(() => null)));
      for (const [i, mine, theirs] of [[0, l, r], [1, r, l]] as const) {
        if (!mine || mine.current.type !== "worktree") continue;
        // Already a version of the same repository on the other side.
        if (theirs && theirs.repo === mine.repo) continue;
        const changed = await api.changedFiles(mine.repo, mine.path || null).catch(() => null);
        if (stale) return;
        if (changed) {
          uncommitted = { side: i, name: mine.path.split("/").pop() || "The file" };
          return;
        }
      }
      if (!stale) uncommitted = null;
    })();
    return () => (stale = true);
  });

  /** The other side becomes the last commit of the file with changes. */
  async function compareWithHead() {
    const u = uncommitted;
    if (!u || !edit) return;
    try {
      const head = await api.versionSpec(await edit[u.side].spec(), { type: "rev", rev: "HEAD" });
      await edit[u.side === 0 ? 1 : 0].apply(head);
    } catch (e) {
      error = String(e);
    }
  }

  async function load(keepPosition = false) {
    menu = null;
    if (restoring?.view && !view) view = restoring.view as View;
    const run = ++loads;
    loading = true;
    error = "";
    changedOnDisk = false;
    const top = viewport?.scrollTop ?? 0;
    try {
      const opened = await api.openFileDiff(session.id, path, $state.snapshot(options), syntaxOverride || null, view, $state.snapshot(flatbufferSchemas));
      // The tab was closed or loaded again meanwhile: nothing shows this diff.
      if (destroyed || run !== loads) {
        api.closeFileDiff(opened.id);
        return;
      }
      const previous = info?.id;
      info = opened;
      view = info.view;
      dirty = [false, false];
      canUndo = canRedo = false;
      editing = null;
      caretWanted = null;
      ahead = [];
      anchor = null;
      resetRows();
      section = -1;
      if (previous !== undefined) api.closeFileDiff(previous);
      await tick();
      if (restoring && viewport) {
        viewport.scrollTop = restoring.scrollTop;
        selected = restoring.selected !== null && restoring.selected < info.rows ? restoring.selected : null;
        restoring = null;
        ensureLoaded();
      } else if (keepPosition && viewport) viewport.scrollTop = top;
      else if (info.sections.length) goTo(0);
      else ensureLoaded();
      if (findOpen && findQuery) runSearch(keepPosition);
    } catch (e) {
      if (run === loads && !destroyed) error = String(e);
    } finally {
      if (run === loads) loading = false;
    }
  }

  function resetRows() {
    menu = null;
    cache = new Map();
    pending = new Set();
    loaded = new Set();
    generation++;
    cacheVersion++;
    ensureLoaded();
  }

  async function fetchChunk(chunk: number) {
    if (!info || pending.has(chunk)) return;
    pending.add(chunk);
    inFlight++;
    const id = info.id;
    const gen = generation;
    let failed = false;
    try {
      const rows = await api.fileRows(id, chunk * CHUNK, CHUNK, theme());
      // Rows of an older diff, from before an edit.
      if (info?.id !== id || gen !== generation) return;
      rows.forEach((row, i) => cache.set(chunk * CHUNK + i, row));
      loaded.add(chunk);
      if (loaded.size > KEEP_CHUNKS) evict();
      cacheVersion++;
      placeCaret();
    } catch (e) {
      failed = true;
      pending.delete(chunk);
      error = String(e);
    } finally {
      inFlight--;
      // The next request goes to what is on screen now. After a failure the
      // next scroll retries, rather than a loop here.
      if (!failed) ensureLoaded();
    }
  }

  /** Drops the loaded chunks farthest from the view, so memory stays bounded in huge files. */
  function evict() {
    const here = Math.floor((projection.source(first) ?? 0) / CHUNK);
    const far = [...loaded].sort((a, b) => Math.abs(b - here) - Math.abs(a - here));
    const wanted = new Set(wantedChunks());
    for (const chunk of far.filter((c) => !wanted.has(c)).slice(0, Math.max(0, loaded.size - KEEP_CHUNKS))) {
      loaded.delete(chunk);
      pending.delete(chunk);
      for (let i = chunk * CHUNK; i < (chunk + 1) * CHUNK; i++) cache.delete(i);
    }
  }

  /** The chunks to load, most needed first: those on screen, then their neighbours. */
  function wantedChunks(): number[] {
    const list: number[] = [];
    if (caretWanted) {
      const c = Math.floor(caretWanted.hint / CHUNK);
      list.push(c);
      if ((c + 1) * CHUNK < total) list.push(c + 1);
      if (c > 0) list.push(c - 1);
    }
    for (let i = first; i < last; i++) {
      const row = projection.source(i);
      if (row !== null) list.push(Math.floor(row / CHUNK));
    }
    for (const i of [first - CHUNK, last + CHUNK]) {
      const row = projection.source(i);
      if (row !== null) list.push(Math.floor(row / CHUNK));
    }
    return [...new Set(list)];
  }

  function ensureLoaded() {
    if (!info) return;
    while (inFlight < PARALLEL) {
      const next = wantedChunks().find((c) => !pending.has(c));
      if (next === undefined) return;
      fetchChunk(next);
    }
  }

  $effect(() => {
    void first;
    void last;
    ensureLoaded();
  });

  /** Scrolls so that row pixel `v` is at the top. */
  function scrollToV(v: number) {
    if (viewport) viewport.scrollTop = Math.max(0, v) / scale;
  }

  function goTo(i: number) {
    if (!info || !viewport || !projectedSections.length) return;
    const target = projectedSections.find((s) => s.index >= i) ?? projectedSections[projectedSections.length - 1];
    section = target.index;
    selected = target.source;
    scrollToV((target.start - 3) * ROW);
    revealMarkWanted = true;
  }

  /** After moving to a difference: the code scrolls sideways so its first change shows. */
  let revealMarkWanted = false;
  function revealMarkSideways() {
    if (!revealMarkWanted || !viewport) return;
    const row = viewport.querySelector<HTMLElement>(".row.cur");
    if (!row) return;
    revealMarkWanted = false;
    const el = row.querySelector<HTMLElement>(".mk");
    const cell = el?.closest<HTMLElement>(".code");
    if (!el || !cell) {
      hscroll = 0;
      return;
    }
    const [a, c] = [el.getBoundingClientRect(), cell.getBoundingClientRect()];
    if (a.left < c.left + 6 || a.left > c.right - 40) {
      hscroll = Math.max(0, Math.min(40000, hscroll + a.left - c.left - c.width / 3));
    }
  }

  let detail = $state<HTMLElement>();
  // The detail lines show the selected row's first change, also in a long line.
  $effect(() => {
    void selectedRow;
    tick().then(() => {
      if (!detail) return;
      const el = detail.querySelector<HTMLElement>(".mk");
      const box = detail.getBoundingClientRect();
      if (!el) detail.scrollLeft = 0;
      else if (el.getBoundingClientRect().right > box.right) {
        detail.scrollLeft += el.getBoundingClientRect().left - box.left - box.width / 3;
      }
    });
  });

  function next(dir: 1 | -1) {
    if (!projectedSections.length) return;
    const current = sectionOrdinals.get(section) ?? -1;
    let at: number;
    if (current < 0 || shownSection !== section) {
      const top = Math.floor(vTop / ROW);
      const idx = projectedSections.findIndex((s) => s.start > top);
      at = dir === 1 ? (idx === -1 ? projectedSections.length - 1 : idx) : Math.max(0, (idx === -1 ? projectedSections.length : idx) - 1);
    } else at = Math.max(0, Math.min(projectedSections.length - 1, current + dir));
    goTo(projectedSections[at].index);
  }

  /** Scrolls an original row into the filtered viewport. */
  function reveal(row: number) {
    if (!viewport || !projection.contains(row)) return;
    const top = projection.position(row) * ROW;
    if (top < vTop || top + ROW > vTop + viewHeight) scrollToV(top - viewHeight / 3);
  }

  // ---- Editing ----

  /** Queues an edit; `changes` is false for one that leaves the text as it is. */
  function enqueue(task: () => Promise<EditInfo>, changes = true): Promise<EditInfo | undefined> {
    const id = info?.id;
    if (changes) unanswered++;
    const run = queue
      .then(task)
      .then(
        (e) => {
          // At once, not after the rows are fetched: saving and closing the tab look at it.
          if (info?.id === id) dirty = e.dirty;
          return e;
        },
        (e) => {
          error = String(e);
          return undefined;
        },
      )
      .finally(() => {
        if (changes) unanswered--;
      });
    queue = run;
    return run;
  }

  /**
   * Sends an edit after which typing goes on where it puts the caret (Enter, a
   * pasted block, joining lines). Until the editor is there, keys are held back
   * rather than typed into the line edited before.
   */
  function caretEdit(task: () => Promise<EditInfo>, then?: (e: EditInfo | undefined) => void) {
    awaiting = true;
    enqueue(task)
      .then(async (e) => {
        await applyEdit(e);
        then?.(e);
      })
      .finally(() => {
        awaiting = false;
        tick().then(replay);
      });
  }

  /** Closes the line editor until `caretWanted` opens it again; the view keeps the keys meanwhile. */
  function detachEditor() {
    const focused = !!input && document.activeElement === input;
    editing = null;
    if (focused) keepFocus();
  }

  /** Edits whose rows are still being fetched; only the latest is shown. */
  let swapToken = 0;

  /**
   * Shows the result of an edit. The rows on screen are fetched first and swapped
   * in together with the new layout, so the view never shows empty rows meanwhile.
   */
  function applyEdit(e: EditInfo | undefined): Promise<void> {
    applied = showEdit(e);
    return applied;
  }

  async function showEdit(e: EditInfo | undefined) {
    if (!e || !info) return;
    showAllRows();
    menu = null;
    const id = info.id;
    const token = ++swapToken;
    const top = Math.max(0, Math.floor(vTop / ROW) - 20);
    const end = Math.min(e.rows, top + Math.ceil(viewHeight / ROW) + 40);
    const chunks: number[] = [];
    for (let c = Math.floor(top / CHUNK); c * CHUNK < end; c++) chunks.push(c);
    let fetched: RowView[][] | null = null;
    try {
      fetched = await Promise.all(chunks.map((c) => api.fileRows(id, c * CHUNK, CHUNK, theme())));
    } catch {
      // Shown as the rows load the usual way.
    }
    if (token !== swapToken || info?.id !== id) return;
    info = {
      ...info,
      rows: e.rows,
      sections: e.sections,
      rowRuns: e.rowRuns,
      stats: e.stats,
      identical: e.identical,
      note: null,
      left: { ...info.left, lines: e.lines[0], exists: e.exists[0], lineEnding: e.lineEndings[0], size: e.sizes[0] },
      right: { ...info.right, lines: e.lines[1], exists: e.exists[1], lineEnding: e.lineEndings[1], size: e.sizes[1] },
    };
    anchor = null;
    dirty = e.dirty;
    canUndo = e.canUndo;
    canRedo = e.canRedo;
    error = "";
    section = -1;
    if (e.focusRow !== null) {
      selected = e.focusRow;
      reveal(e.focusRow);
    }
    if (e.caret && editing) {
      caretWanted = { side: editing.side, line: e.caret.line, col: e.caret.col, hint: e.caret.row };
      detachEditor();
      reveal(e.caret.row);
    }
    if (fetched) {
      cache = new Map();
      fetched.forEach((rows, k) => rows.forEach((row, i) => cache.set(chunks[k] * CHUNK + i, row)));
      loaded = new Set(chunks);
      pending = new Set(chunks);
      generation++;
      cacheVersion++;
      ensureLoaded();
      placeCaret();
    } else {
      resetRows();
    }
    if (findOpen && findQuery) runSearch(true);
  }

  /** Opens the line editor on a row once the row with the wanted line is loaded. */
  function placeCaret() {
    const want = caretWanted;
    if (!want) return;
    for (let d = 0; d < 400; d++) {
      for (const i of d ? [want.hint + d, want.hint - d] : [want.hint]) {
        const row = cache.get(i);
        const line = row && (want.side === 0 ? row.l : row.r);
        if (line?.n === want.line) {
          caretWanted = null;
          openEditor(want.side, i, want.col);
          // Also when the editor could not open: the held keys then go to the view.
          tick().then(replay);
          return;
        }
      }
    }
    // Not near where it was expected, with the rows around loaded: the editor stays
    // closed, rather than holding back the keys for good.
    const c = Math.floor(want.hint / CHUNK);
    const near = [c - 1, c, c + 1].filter((k) => k >= 0 && k * CHUNK < total);
    if (near.every((k) => loaded.has(k))) {
      caretWanted = null;
      tick().then(replay);
    }
  }

  function lineOf(side: 0 | 1, row: RowView | undefined): LineView | null | undefined {
    return row && (side === 0 ? row.l : row.r);
  }

  /** Opens the line editor with the caret at `col` (the end without), typing `insert` there. */
  function openEditor(side: 0 | 1, index: number, col?: number, insert = "") {
    const row = cache.get(index);
    // Two empty files have no rows: their first line is typed as a new one.
    const empty = info?.rows === 0 && index === 0;
    if ((!row && !empty) || !editable[side]) return;
    const line = lineOf(side, row);
    if (line?.x || line?.o) {
      error = "This line is too long to edit here.";
      return;
    }
    showAllRows();
    activeSide = side;
    selected = index;
    editing = { side, row: index, line: line ? line.n : null };
    draft = original = line?.t ?? "";
    let at = Math.min(col ?? draft.length, draft.length);
    if (insert) {
      draft = draft.slice(0, at) + insert + draft.slice(at);
      at += insert.length;
    }
    inputReady = false;
    tick().then(() => {
      input?.focus();
      input?.setSelectionRange(at, at);
      inputReady = true;
      replay();
    });
  }

  /** The UTF-16 column under the mouse in a code cell. */
  function columnAt(e: MouseEvent, cell: HTMLElement): number | undefined {
    const doc = document as Document & { caretRangeFromPoint?: (x: number, y: number) => Range | null };
    const hit = doc.caretRangeFromPoint?.(e.clientX, e.clientY);
    const text = cell.querySelector(".text");
    if (!hit || !text || !text.contains(hit.startContainer)) return undefined;
    const range = document.createRange();
    range.setStart(text, 0);
    range.setEnd(hit.startContainer, hit.startOffset);
    return range.toString().length;
  }

  /** A click in an editable line puts the caret there, as in an editor. Shift+click
   *  selects lines, and a click after selecting text with the mouse keeps the selection. */
  function clickCode(e: MouseEvent, side: 0 | 1, index: number) {
    activeSide = side;
    if (!editable[side] || e.shiftKey || e.ctrlKey || e.metaKey || textSelected()) return;
    // The row's own click would take the focus back to the view.
    e.stopPropagation();
    // An edit is on its way and moves rows: the click would land on the wrong line.
    if (waiting()) return;
    anchor = null;
    const col = columnAt(e, e.currentTarget as HTMLElement);
    const line = lineOf(side, cache.get(index));
    if (editing && draft !== original && line) {
      // The line being edited is sent first, which can move rows; the caret then
      // goes to the clicked line by its number.
      const want = { side, line: line.n, col: col ?? END, hint: index };
      commit(draft, null, () => {
        caretWanted = want;
        placeCaret();
      });
      return;
    }
    editing = null;
    openEditor(side, index, col);
  }

  /** Sends the typed line; `text` may hold line breaks, `caret` where typing goes on. */
  function commit(text: string, caret: number | null, then?: (e: EditInfo | undefined) => void) {
    const ed = editing;
    if (!ed || !info) return;
    const id = info.id;
    const side = SIDE_NAMES[ed.side];
    if (caret === null) detachEditor();
    if (text === original && caret === null) return;
    const task = () =>
      ed.line === null
        ? api.insertLine(id, side, ed.row, text)
        : api.editText(id, side, { line: ed.line, col: 0 }, { line: ed.line, col: END }, text, caret);
    if (caret === null && !then) enqueue(task).then(applyEdit);
    else caretEdit(task, then);
  }

  function editKey(e: KeyboardEvent) {
    const ed = editing;
    if (!ed || !info || !input || holdKey(e)) return;
    const [a, b] = [input.selectionStart ?? 0, input.selectionEnd ?? 0];
    const id = info.id;
    const side = SIDE_NAMES[ed.side];
    const mod = e.ctrlKey || e.metaKey;
    if (e.key === "Enter") {
      commit(draft.slice(0, a) + "\n" + draft.slice(b), a + 1);
    } else if (e.key === "Escape") {
      // Leaves the line; what was typed stays.
      commit(draft, null);
      viewport?.focus();
    } else if ((e.key === "ArrowUp" || e.key === "ArrowDown") && !e.shiftKey && !mod) {
      if (!toLine(ed, e.key === "ArrowUp" ? -1 : 1, a)) return;
    } else if (e.key === "ArrowLeft" && a === 0 && b === 0 && !e.shiftKey && !mod) {
      // Past the start of the line: the end of the one above.
      if (!toLine(ed, -1, END)) return;
    } else if (e.key === "ArrowRight" && a === draft.length && b === a && !e.shiftKey && !mod) {
      if (!toLine(ed, 1, 0)) return;
    } else if (e.key === "Backspace" && a === 0 && b === 0 && ed.line !== null && ed.line > 0) {
      // Joins the line to the one above.
      const line = ed.line;
      caretEdit(() => api.editText(id, side, { line: line - 1, col: END }, { line, col: END }, draft, 0));
    } else if (e.key === "Delete" && a === draft.length && b === a && ed.line !== null) {
      // Joins the line below to this one.
      const line = ed.line;
      const sideLines = ed.side === 0 ? info.left.lines : info.right.lines;
      if (line + 1 >= sideLines) return;
      caretEdit(() => api.editText(id, side, { line, col: 0 }, { line: line + 1, col: 0 }, draft, a));
    } else if (e.key === "Tab" && !mod) {
      typeText("\t");
    } else if (mod && e.key === "s") {
      commit(draft, null);
      save();
    } else if (mod && (e.key === "z" || e.key === "y") && draft === original) {
      editing = null;
      if (e.key === "z" && !e.shiftKey) undo();
      else redo();
    } else {
      return;
    }
    e.preventDefault();
    e.stopPropagation();
  }

  /** Moves the editor to the line above (`-1`) or below, caret at `col`; false at the edge. */
  function toLine(ed: NonNullable<typeof editing>, dir: -1 | 1, col: number): boolean {
    if (!info) return false;
    const target = ed.line === null ? null : ed.line + dir;
    const sideLines = ed.side === 0 ? info.left.lines : info.right.lines;
    if (target === null || target < 0 || target >= sideLines) return false;
    const move = (hint: number) => {
      caretWanted = { side: ed.side, line: target, col, hint: hint + dir };
      detachEditor();
      placeCaret();
    };
    if (draft === original) move(ed.row);
    else commit(draft, Math.min(col, draft.length), (r) => r?.caret && move(r.caret.row));
    return true;
  }

  function onPaste(e: ClipboardEvent) {
    if (holdPaste(e)) return;
    const text = e.clipboardData?.getData("text/plain") ?? "";
    if (!input || !/[\r\n]/.test(text)) return;
    e.preventDefault();
    // Handled: the view around the editor must not hold it back to paste it again.
    e.stopPropagation();
    pasteText(text);
  }

  /** Pastes into the line editor; several lines replace the selection and become lines of the file. */
  function pasteText(text: string) {
    if (!input) return;
    if (!/[\r\n]/.test(text)) return typeText(text);
    const [a, b] = [input.selectionStart ?? 0, input.selectionEnd ?? 0];
    commit(draft.slice(0, a) + text + draft.slice(b), a + text.length);
  }

  // ---- Typing ahead ----
  // An edit that moves the caret takes a moment, and so does loading the row the
  // caret goes to. Keys typed meanwhile are held back and typed, in order, into
  // the editor where the edit puts it: else they would go to the line edited
  // before, and be lost when the editor moves.

  const waiting = () => awaiting || caretWanted !== null;
  /** Keys held back besides the characters: those that edit, move the caret or leave the line. */
  const AHEAD_KEYS = new Set(["Enter", "Escape", "Tab", "Backspace", "Delete", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "F2"]);
  /** With Ctrl: save, undo, redo, select all. Others (copy, close the tab) work as usual. */
  const AHEAD_MOD_KEYS = new Set(["s", "z", "Z", "y", "a"]);

  /** Holds a key back while the editor waits; true when it did. */
  function holdKey(e: KeyboardEvent): boolean {
    if (!waiting() || e.altKey) return false;
    const held = e.ctrlKey || e.metaKey ? AHEAD_MOD_KEYS.has(e.key) : e.key.length === 1 || AHEAD_KEYS.has(e.key);
    if (!held) return false;
    ahead = [...ahead, { key: e.key, shift: e.shiftKey, ctrl: e.ctrlKey, meta: e.metaKey }];
    e.preventDefault();
    e.stopPropagation();
    return true;
  }

  function holdPaste(e: ClipboardEvent): boolean {
    if (!waiting()) return false;
    const text = e.clipboardData?.getData("text/plain") ?? "";
    if (text) ahead = [...ahead, { paste: text }];
    e.preventDefault();
    e.stopPropagation();
    return true;
  }

  /** Types the held keys once the editor is back; stops when one of them makes it wait again. */
  function replay() {
    while (ahead.length && !waiting() && !(editing && !inputReady)) {
      const [next, ...rest] = ahead;
      ahead = rest;
      if ("paste" in next) {
        if (editing) pasteText(next.paste);
        else if (selected !== null && editable[activeSide]) replaceLines(next.paste, anchor !== null);
        continue;
      }
      const e = new KeyboardEvent("keydown", { key: next.key, shiftKey: next.shift, ctrlKey: next.ctrl, metaKey: next.meta, cancelable: true });
      if (!editing) onKey(e);
      else {
        editKey(e);
        // What the input itself does with a key the editor leaves to it.
        if (!e.defaultPrevented) typeKey(e);
      }
    }
  }

  /** Sets the editor's text and selection at once, not waiting for the binding. */
  function setDraft(text: string, from: number, to = from, dir: "forward" | "backward" = "forward") {
    draft = text;
    if (!input) return;
    input.value = text;
    input.setSelectionRange(from, to, dir);
  }

  /** Types `text` over the editor's selection. */
  function typeText(text: string) {
    if (!input) return;
    const [a, b] = [input.selectionStart ?? 0, input.selectionEnd ?? 0];
    setDraft(draft.slice(0, a) + text + draft.slice(b), a + text.length);
  }

  /** A held key, as the input would have handled it: characters, deleting, moving the caret. */
  function typeKey(e: KeyboardEvent) {
    if (!input) return;
    const mod = e.ctrlKey || e.metaKey;
    const [a, b] = [input.selectionStart ?? 0, input.selectionEnd ?? 0];
    const back = input.selectionDirection === "backward";
    const [fixed, moving] = back ? [b, a] : [a, b];
    // Shift extends the selection from its fixed end.
    const moveTo = (to: number) =>
      e.shiftKey ? setDraft(draft, Math.min(fixed, to), Math.max(fixed, to), to < fixed ? "backward" : "forward") : setDraft(draft, to);
    if (e.key.length === 1 && !mod) typeText(e.key);
    else if (mod && e.key === "a") setDraft(draft, 0, draft.length);
    else if ((e.key === "Backspace" || e.key === "Delete") && a !== b) typeText("");
    else if (e.key === "Backspace" && a > 0) setDraft(draft.slice(0, a - 1) + draft.slice(a), a - 1);
    else if (e.key === "Delete") setDraft(draft.slice(0, a) + draft.slice(a + 1), a);
    else if (e.key === "ArrowLeft") moveTo(e.shiftKey ? Math.max(0, moving - 1) : a !== b ? a : Math.max(0, a - 1));
    else if (e.key === "ArrowRight") moveTo(e.shiftKey ? Math.min(draft.length, moving + 1) : a !== b ? b : Math.min(draft.length, b + 1));
    else if (e.key === "Home") moveTo(0);
    else if (e.key === "End") moveTo(draft.length);
  }

  function copySection(i: number, to: 0 | 1) {
    if (!info || i < 0 || !editable[to]) return;
    showAllRows();
    const s = info.sections[i];
    const id = info.id;
    editing = null;
    enqueue(() => api.copyRows(id, s.start, s.end, SIDE_NAMES[to])).then(applyEdit);
  }

  /** Whether text is selected with the mouse in the view; Ctrl+C copies that then. */
  function textSelected(): boolean {
    const sel = window.getSelection();
    return !!sel && !sel.isCollapsed && !!viewport?.contains(sel.anchorNode);
  }

  /** Copies the selected lines of the active side, and with `cut` deletes them. */
  async function copyLines(cut: boolean, side = activeSide, r = lineRange) {
    if (!info || !r) return;
    const id = info.id;
    try {
      const text = await api.rowsText(id, SIDE_NAMES[side], r[0], r[1]);
      if (!text) return;
      await navigator.clipboard.writeText(text);
      if (cut) replaceLines("", true);
    } catch (e) {
      error = String(e);
    }
  }

  /** Keep the native text menu while typing into the editor. Diff rows have their own menu. */
  function rowContext(e: MouseEvent, index: number) {
    if (editing || waiting() || unanswered || loading || !info) return;
    const target = (e.target as Element).closest<HTMLElement>(".code[data-side], .ln[data-side]");
    if (!target) return;
    e.preventDefault();
    const side = target.dataset.side === "left" ? 0 : 1;
    const selection = window.getSelection();
    const cellOf = (node: Node | null) => (node instanceof Element ? node : node?.parentElement)?.closest<HTMLElement>(".code[data-side]");
    const from = cellOf(selection?.anchorNode ?? null), to = cellOf(selection?.focusNode ?? null);
    const text = selection && !selection.isCollapsed && from?.dataset.side === target.dataset.side && to?.dataset.side === target.dataset.side
      && selection.getRangeAt(0).intersectsNode(target) ? selection.toString() : "";
    if (!text) selection?.removeAllRanges();
    rowMenu(index, side, e.clientX, e.clientY, text);
  }

  function rowMenu(index: number, side: 0 | 1, x: number, y: number, selectedText = "") {
    if (!info || !cache.has(index) || editing || waiting() || unanswered || loading) return;
    if (activeSide !== side || !lineRange || index < lineRange[0] || index >= lineRange[1]) {
      anchor = null;
      selected = index;
    }
    activeSide = side;
    const range: [number, number] = lineRange ? [...lineRange] : [index, index + 1];
    const many = range[1] - range[0] > 1;
    const name = SIDE_NAMES[side], other = SIDE_NAMES[1 - side], destination = (1 - side) as 0 | 1;
    const source = side === 0 ? session.left : session.right;
    const line = lineOf(side, cache.get(index));
    const at = info.sections.findIndex((s) => s.start <= index && index < s.end);
    const id = info.id, epoch = generation;
    const run = (action: () => void) => () => {
      if (info?.id === id && generation === epoch && !waiting() && !unanswered
        && activeSide === side && lineRange?.[0] === range[0] && lineRange[1] === range[1]) action();
    };
    const items: MenuItem[] = [{
      label: `Copy difference to ${other} ${side === 0 ? "→" : "←"}`,
      detail: at < 0 ? "No difference at this line" : !editable[destination] ? `The ${other} side is read-only in this view` : `Replaces the ${other}-hand difference`,
      keys: side === 0 ? "Alt+→" : "Alt+←", tone: "primary",
      disabled: at < 0 || !editable[destination], run: run(() => copySection(at, destination)),
    }, "separator", {
      label: selectedText ? "Copy selected text" : many ? "Copy selected lines" : "Copy line to clipboard",
      keys: "Ctrl+C", disabled: !selectedText && !many && !line,
      run: run(() => selectedText ? copyText(selectedText) : copyLines(false, side, range)),
    }, {
      label: "Select all lines", keys: "Ctrl+A", run: run(() => { showAllRows(); window.getSelection()?.removeAllRanges(); anchor = 0; selected = total - 1; }),
    }, "separator"];
    const reason = editable[side] ? undefined : `The ${name} side is read-only in this view`;
    items.push({
      label: "Edit line…", keys: "F2", detail: reason ?? (many ? "Select a single line to edit" : line?.x || line?.o ? "This line is too long to edit here" : undefined),
      disabled: !editable[side] || many || !!line?.x || !!line?.o,
      run: run(() => { anchor = null; openEditor(side, index); }),
    }, {
      label: many ? `Delete selected lines on ${name}` : `Delete ${name} line`,
      detail: reason, tone: "danger", disabled: !editable[side] || (!many && !line),
      run: run(() => replaceLines("", true)),
    });
    if (dirty[side] && editable[side]) items.push({ label: `Save ${name} file`, run: run(() => save([side])) });
    items.push("separator", { label: "Copy file path", run: () => copyFilePath(side) }, { label: "Find in both files…", keys: "Ctrl+F", run: openFind });
    const atLine = line ? `Line ${line.n + 1}` : `No line on the ${name}`;
    const where = many ? `${atLine} · ${range[1] - range[0]} comparison rows selected` : atLine;
    menu = { x, y, items, context: {
      side: name, title: path.split("/").pop() || source.display.split(/[\\/]/).pop() || source.display,
      detail: `${source.display}${path ? ` › ${path}` : ""}\n${where}`, badge: editable[side] ? "Editable" : "Read-only",
    } };
  }

  async function copyText(text: string) {
    try { await navigator.clipboard.writeText(text); } catch (e) { error = String(e); }
  }

  async function copyFilePath(side: 0 | 1) {
    try { await copyText(sourceText(await api.sideSource(session.id, side === 0, path))); }
    catch (e) { error = String(e); }
  }

  async function keyboardMenu() {
    if (!info || selected === null || !viewport) return;
    const id = info.id, index = selected, side = activeSide, epoch = generation;
    try {
      // Ctrl+A can leave the current row outside the rendered window.
      if (!cache.has(index)) {
        const [row] = await api.fileRows(id, index, 1, theme());
        if (info?.id !== id || selected !== index || activeSide !== side || generation !== epoch || !row) return;
        cache.set(index, row);
        cacheVersion++;
      }
      const r = viewport.getBoundingClientRect();
      const y = Math.max(r.top, Math.min(r.bottom - ROW, r.top + projection.position(index) * ROW - vTop)) + ROW;
      rowMenu(index, side, r.left + (side === 0 ? 40 : r.width / 2 + 40), y);
    } catch (e) { error = String(e); }
  }

  /** Replaces the selected lines of the active side, or inserts before the current one. */
  function replaceLines(text: string, replace: boolean) {
    const r = lineRange;
    if (!info || !r || !editable[activeSide]) return;
    const id = info.id;
    const side = SIDE_NAMES[activeSide];
    anchor = null;
    selected = r[0];
    editing = null;
    enqueue(() => api.replaceRows(id, side, r[0], r[1], text, replace)).then(applyEdit);
  }

  /** Ctrl+V outside the line editor: lines go above the current one, or replace the selected ones. */
  function onViewPaste(e: ClipboardEvent) {
    if (e.defaultPrevented || holdPaste(e)) return;
    if (editing || selected === null || !editable[activeSide]) return;
    const text = e.clipboardData?.getData("text/plain") ?? "";
    if (!text) return;
    e.preventDefault();
    replaceLines(text, anchor !== null);
  }

  /** Line endings the other side can take over: both sides have a single, different kind. */
  const eolFix = $derived.by(() => {
    if (!info || info.view !== "text" || !info.left.exists || !info.right.exists) return null;
    const single = (e: string) => e === "LF" || e === "CRLF" || e === "CR";
    const [l, r] = [info.left.lineEnding, info.right.lineEnding];
    if (l === r || (!single(l) && !single(r))) return null;
    // A side without any line break has nothing to convert.
    const fix = {
      left: single(r) && l !== "None" && editable[0] ? r : null,
      right: single(l) && r !== "None" && editable[1] ? l : null,
    } as { left: "LF" | "CRLF" | "CR" | null; right: "LF" | "CRLF" | "CR" | null };
    return fix.left || fix.right ? fix : null;
  });

  function setLineEndings(side: 0 | 1, ending: "LF" | "CRLF" | "CR") {
    if (!info) return;
    const id = info.id;
    editing = null;
    enqueue(() => api.setLineEndings(id, SIDE_NAMES[side], ending)).then(applyEdit);
  }

  function undo() {
    if (!info) return;
    const id = info.id;
    editing = null;
    enqueue(() => api.undo(id)).then(applyEdit);
  }

  function redo() {
    if (!info) return;
    const id = info.id;
    editing = null;
    enqueue(() => api.redo(id)).then(applyEdit);
  }

  /** Saves the changed sides among `sides` (both by default). */
  async function save(sides: (0 | 1)[] = [0, 1]) {
    if (!info) return;
    const id = info.id;
    // The line being typed is an edit too; typed edits land first, so they are saved too.
    if (editing && draft !== original && !awaiting) commit(draft, null);
    await queue;
    for (const side of sides) {
      if (!dirty[side]) continue;
      busy = "Saving…";
      try {
        let saved = await api.saveSide(id, SIDE_NAMES[side], false);
        if (saved.stale) {
          busy = "";
          const name = side === 0 ? "left" : "right";
          const replace = await ask(
            `The ${name} file changed on disk after it was opened. Replace it with your version, losing the other changes?`,
            { title: "File changed on disk", kind: "warning", okLabel: "Replace", cancelLabel: "Keep the file" },
          );
          if (!replace) {
            changedOnDisk = true;
            continue;
          }
          busy = "Saving…";
          saved = await api.saveSide(id, SIDE_NAMES[side], true);
        }
        applyEdit(saved);
        if (!saved.dirty[0] && !saved.dirty[1]) changedOnDisk = false;
      } catch (e) {
        error = String(e);
        return;
      } finally {
        busy = "";
      }
    }
  }

  /** Whitespace, case and grammar changes keep the edits. */
  let pathbars = $state<PathBar[]>([]);
  const hasSyntax = $derived(
    info?.view === "text" ||
      info?.view === "prettyXml" ||
      info?.view === "prettyJson" ||
      info?.view === "chatTemplate" ||
      info?.view === "reflowedText",
  );
  const optionsChanged = $derived.by(() => {
    const d = defaultDiffOptions();
    return (
      options.whitespace !== d.whitespace || options.ignoreCase !== d.ignoreCase || options.histogram !== d.histogram || !!syntaxOverride
    );
  });

  /** What this view offers in the command palette. */
  export function commands(): Command[] {
    const group = "Text";
    const text = !!info && info.view !== "image";
    const list: Command[] = [
      { group, title: "Previous difference", keys: "F7", enabled: text && !!projectedSections.length, run: () => next(-1) },
      { group, title: "Next difference", keys: "F8", enabled: text && !!projectedSections.length, run: () => next(1) },
      { group, title: "Find in both files", keys: "Ctrl+F", enabled: text, run: openFind },
    ];
    if (text) list.push(...rowFilters.map((f) => ({ group, title: `Show: ${f.label}`, run: () => setRowFilter(f.id as RowFilter) })));
    if (onSwap) list.push({ group, title: "Swap left and right", run: onSwap });
    if (editable[0] || editable[1]) {
      list.push(
        { group, title: "Copy difference to the left", keys: "Alt+←", enabled: editable[0] && currentSection >= 0, run: () => copySection(currentSection, 0) },
        { group, title: "Copy difference to the right", keys: "Alt+→", enabled: editable[1] && currentSection >= 0, run: () => copySection(currentSection, 1) },
        { group, title: "Undo", keys: "Ctrl+Z", enabled: canUndo, run: undo },
        { group, title: "Redo", keys: "Ctrl+Y", enabled: canRedo, run: redo },
        { group, title: "Save all changed sides", keys: "Ctrl+S", enabled: anyDirty, run: () => save() },
        { group, title: "Save left side only", enabled: dirty[0], run: () => save([0]) },
        { group, title: "Save right side only", enabled: dirty[1], run: () => save([1]) },
      );
    }
    const ws = [["exact", "Whitespace: exact"], ["normalize", "Whitespace: ignore changes"], ["ignoreAll", "Whitespace: ignore all"]] as const;
    for (const [id, title] of ws) {
      list.push({
        group,
        title,
        enabled: options.whitespace !== id,
        run: () => {
          options.whitespace = id;
          settingsChanged();
        },
      });
    }
    list.push(
      {
        group,
        title: options.ignoreCase ? "Match case" : "Ignore case",
        run: () => {
          options.ignoreCase = !options.ignoreCase;
          settingsChanged();
        },
      },
      {
        group,
        title: options.histogram ? "Use the Myers diff" : "Use the histogram diff",
        run: () => {
          options.histogram = !options.histogram;
          settingsChanged();
        },
      },
    );
    for (const v of info?.views ?? []) {
      if (v.id === view) continue;
      list.push({
        group,
        title: `Show as: ${v.label}`,
        enabled: !anyDirty,
        run: () => {
          view = v.id;
          syntaxOverride = "";
          load();
        },
      });
    }
    return [...list, ...pathbars.flatMap((bar) => bar?.commands() ?? [])];
  }

  function settingsChanged() {
    if (!info || (!anyDirty && !canUndo)) {
      load(true);
      return;
    }
    const id = info.id;
    enqueue(() => api.updateDiff(id, $state.snapshot(options), syntaxOverride || null), false).then(applyEdit);
  }

  async function reloadFromDisk() {
    if (anyDirty) {
      const ok = await ask("Reload the file from disk and discard your unsaved edits?", { title: "Reload", kind: "warning" });
      if (!ok) return;
    }
    load(true);
  }

  // ---- Find ----

  function openFind() {
    if (!info || info.view === "image") return;
    findOpen = true;
    // Selected text in the view becomes the query.
    const picked = window.getSelection()?.toString() ?? "";
    if (picked && !picked.includes("\n") && viewport?.contains(window.getSelection()?.anchorNode ?? null)) {
      findQuery = picked;
      runSearch(false);
    }
    tick().then(() => {
      findInput?.focus();
      findInput?.select();
    });
  }

  function closeFind() {
    findOpen = false;
    hits = [];
    hitIndex = -1;
    findError = "";
    clearTimeout(searchTimer);
    searchRun++;
    viewport?.focus();
  }

  /** Searches again; `keep` stays near the current match instead of the view. */
  async function runSearch(keep: boolean) {
    clearTimeout(searchTimer);
    const run = ++searchRun;
    if (!info || !findQuery) {
      hits = [];
      hitIndex = -1;
      findError = "";
      truncated = false;
      return;
    }
    const from = keep && currentHit ? currentHit : { row: selected ?? Math.floor(vTop / ROW), side: 0 as const, start: 0 };
    try {
      const r = await api.searchDiff(info.id, findQuery, $state.snapshot(findOptions));
      if (run !== searchRun) return;
      findError = "";
      hits = r.hits;
      truncated = r.truncated;
      // The first match at or after where the reader is.
      const at = hits.findIndex((h) => h.row > from.row || (h.row === from.row && (h.side > from.side || (h.side === from.side && h.start >= from.start))));
      if (!hits.length) hitIndex = -1;
      else if (keep) hitIndex = at === -1 ? hits.length - 1 : at;
      else showHit(at === -1 ? 0 : at);
    } catch (e) {
      if (run !== searchRun) return;
      findError = String(e);
      hits = [];
      hitIndex = -1;
    }
  }

  function searchSoon() {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => runSearch(false), 150);
  }

  function showHit(i: number) {
    const h = hits[i];
    if (!h) return;
    if (!projection.contains(h.row)) showAllRows();
    hitIndex = i;
    selected = h.row;
    activeSide = h.side;
    section = -1;
    if (viewport) {
      tick().then(() => reveal(h.row));
    }
    revealHitWanted = true;
    tick().then(revealHitSideways);
  }

  function nextHit(dir: 1 | -1) {
    if (!hits.length) return;
    if (hitIndex === -1) {
      runSearch(false);
      return;
    }
    showHit((hitIndex + dir + hits.length) % hits.length);
  }

  /** Scrolls the code sideways so the current match is visible. */
  function revealHitSideways() {
    if (!revealHitWanted || !viewport) return;
    const el = viewport.querySelector<HTMLElement>(".hit.now");
    const cell = el?.closest<HTMLElement>(".code");
    if (!el || !cell) return;
    revealHitWanted = false;
    const [a, c] = [el.getBoundingClientRect(), cell.getBoundingClientRect()];
    if (a.left < c.left + 6 || a.right > c.right) {
      hscroll = Math.max(0, Math.min(40000, hscroll + a.left - c.left - c.width / 3));
    }
  }

  // The row is drawn after the scroll, and later still when it was not loaded yet.
  $effect(() => {
    void cacheVersion;
    void first;
    tick().then(() => {
      revealHitSideways();
      revealMarkSideways();
    });
  });

  function findKey(e: KeyboardEvent) {
    if (e.key === "Enter") nextHit(e.shiftKey ? -1 : 1);
    else if (e.key === "Escape") closeFind();
    else return;
    e.preventDefault();
  }

  function toggle(key: keyof SearchOptions) {
    findOptions[key] = !findOptions[key];
    runSearch(false);
    findInput?.focus();
  }

  function onWindowKey(e: KeyboardEvent) {
    // Only the visible tab answers.
    if (!root?.offsetParent) return;
    const mod = e.ctrlKey || e.metaKey;
    if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "f") openFind();
    else if (e.key === "F3" && findOpen) nextHit(e.shiftKey ? -1 : 1);
    else if (mod && e.key.toLowerCase() === "g" && findOpen) nextHit(e.shiftKey ? -1 : 1);
    // Saving works from anywhere in the tab: the find field, the toolbar, the path bars.
    else if (mod && !e.altKey && e.key.toLowerCase() === "s" && !e.defaultPrevented) save();
    // F7/F8 also when the focus has left the scroll area (which handles them otherwise), but not in a field.
    else if ((e.key === "F7" || e.key === "F8") && !e.defaultPrevented && !isField(e.target))
      next(e.key === "F8" ? 1 : -1);
    else return;
    e.preventDefault();
  }

  // ---- Keys, mouse ----

  const isField = (t: EventTarget | null) =>
    t instanceof HTMLInputElement || t instanceof HTMLTextAreaElement || t instanceof HTMLSelectElement;

  /**
   * Keeps keyboard focus on the scroll area: a clicked row would take it, and lose
   * it for good once scrolling takes that row out of the page.
   */
  function keepFocus() {
    viewport?.focus({ preventScroll: true });
  }

  function onKey(e: KeyboardEvent) {
    if (e.altKey && !e.ctrlKey && e.key.toLowerCase() === "h" && uncommitted) {
      e.preventDefault();
      compareWithHead();
      return;
    }
    // Keys typed into the line editor.
    if (e.target instanceof HTMLInputElement || holdKey(e)) return;
    const mod = e.ctrlKey || e.metaKey;
    if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
      keyboardMenu();
      e.preventDefault();
      return;
    }
    // Two empty files: typing, Enter or F2 starts the first line.
    if (info?.rows === 0 && info.view === "text" && !mod && !e.altKey && (e.key.length === 1 || e.key === "Enter" || e.key === "F2")) {
      const side = editable[activeSide] ? activeSide : activeSide === 0 ? 1 : 0;
      if (editable[side]) {
        e.preventDefault();
        openEditor(side, 0, undefined, e.key.length === 1 ? e.key : "");
        return;
      }
    }
    // Typing on a selected line of an editable side edits it, from its end.
    if (e.key.length === 1 && !mod && !e.altKey && selected !== null && editable[activeSide]) {
      e.preventDefault();
      openEditor(activeSide, selected, undefined, e.key);
      return;
    }
    // n and p are shortcuts only where typing does not edit.
    const letters = !editable[activeSide];
    if (e.key === "F8" || (e.altKey && e.key === "ArrowDown") || (letters && e.key === "n")) next(1);
    else if (e.key === "F7" || (e.altKey && e.key === "ArrowUp") || (letters && e.key === "p")) next(-1);
    // Space does not scroll the view.
    else if (e.key === " ") {
      e.preventDefault();
      return;
    }
    else if (e.altKey && e.key === "ArrowRight") copySection(currentSection, 1);
    else if (e.altKey && e.key === "ArrowLeft") copySection(currentSection, 0);
    else if (mod && e.key === "z" && !e.shiftKey) undo();
    else if (mod && (e.key === "y" || (e.key === "Z" && e.shiftKey) || (e.key === "z" && e.shiftKey))) redo();
    else if (mod && e.key === "s") save();
    else if (mod && e.key.toLowerCase() === "a" && total) {
      showAllRows();
      anchor = 0;
      selected = total - 1;
    } else if (mod && e.key === "c" && selected !== null && !textSelected()) copyLines(false);
    else if (mod && e.key === "x" && selected !== null && editable[activeSide]) copyLines(true);
    else if ((e.key === "Delete" || e.key === "Backspace") && !mod && selected !== null && editable[activeSide])
      replaceLines("", true);
    else if ((e.key === "Enter" || e.key === "F2") && selected !== null) openEditor(activeSide, selected);
    else if (e.key === "ArrowDown" && selected !== null) {
      if (e.shiftKey) showAllRows();
      anchor = e.shiftKey ? (anchor ?? selected) : null;
      selected = projection.source(Math.min(displayTotal - 1, projection.position(selected) + 1)) ?? selected;
      reveal(selected);
    } else if (e.key === "ArrowUp" && selected !== null) {
      if (e.shiftKey) showAllRows();
      anchor = e.shiftKey ? (anchor ?? selected) : null;
      selected = projection.source(Math.max(0, projection.position(selected) - 1)) ?? selected;
      reveal(selected);
    } else if (e.key === "PageDown" || e.key === "PageUp") {
      const page = Math.max(1, Math.floor(viewHeight / ROW) - 2) * (e.key === "PageDown" ? 1 : -1);
      if (selected !== null) selected = projection.source(Math.max(0, Math.min(displayTotal - 1, projection.position(selected) + page))) ?? selected;
      scrollToV(vTop + page * ROW);
    } else if (e.key === "Home" || e.key === "End") {
      const end = e.key === "End";
      if (selected !== null) selected = projection.source(end ? displayTotal - 1 : 0);
      scrollToV(end ? displayTotal * ROW : 0);
    } else return;
    e.preventDefault();
  }

  function onWheel(e: WheelEvent) {
    if (viewport && scaledWheel(viewport, scale, e, ROW)) {
      e.preventDefault();
      return;
    }
    const dx = e.shiftKey ? e.deltaY : e.deltaX;
    if (dx) {
      hscroll = Math.max(0, Math.min(40000, hscroll + dx));
      e.preventDefault();
    }
  }

  function overviewClick(e: MouseEvent) {
    if (!viewport || !displayTotal) return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const row = Math.floor(((e.clientY - rect.top) / rect.height) * displayTotal);
    scrollToV(row * ROW - viewHeight / 2);
    section = -1;
  }

  interface Segment {
    text: string;
    color: string | null;
    mark: boolean;
    /** Part of a search match; `now` for the current one. */
    hit: boolean;
    now: boolean;
  }

  /** Splits a line at every syntax-colour, change-mark and match boundary. */
  function segments(line: LineView, hits: Hit[] = []): Segment[] {
    // Matches count from the start of the line; a long line is shown from `o` on.
    const o = line.o ?? 0;
    const found = hits
      .map((h) => ({ hit: h, start: Math.max(0, h.start - o), end: Math.min(line.t.length, h.end - o) }))
      .filter((h) => h.start < h.end);
    const cuts = new Set<number>([0, line.t.length]);
    for (const [a, b] of line.s) cuts.add(a).add(b);
    for (const [a, b] of line.m) cuts.add(a).add(b);
    for (const h of found) cuts.add(h.start).add(h.end);
    const points = [...cuts].filter((p) => p <= line.t.length).sort((a, b) => a - b);
    const out: Segment[] = [];
    let si = 0;
    for (let i = 0; i + 1 < points.length; i++) {
      const [a, b] = [points[i], points[i + 1]];
      while (si < line.s.length && line.s[si][1] <= a) si++;
      const span = si < line.s.length && line.s[si][0] <= a ? line.s[si] : null;
      out.push({
        text: line.t.slice(a, b),
        color: span ? "#" + span[2].toString(16).padStart(6, "0") : null,
        mark: line.m.some(([ma, mb]) => ma <= a && b <= mb),
        hit: found.some((h) => h.start <= a && b <= h.end),
        now: !!currentHit && found.some((h) => h.hit === currentHit && h.start <= a && b <= h.end),
      });
    }
    return out;
  }

  const eolLabel = (s: FileDiffInfo["left"]) => {
    if (!s.exists) return "missing";
    if (info?.view === "image") return formatSize(s.size);
    const eol = s.lineEnding === "None" ? "no line break" : s.lineEnding === "Mixed" ? "mixed line endings" : s.lineEnding;
    const kind = info?.view === "text" ? `${s.encoding}${s.bom ? " BOM" : ""} · ${eol}` : s.encoding;
    return `${kind} · ${formatSize(s.size)} · ${s.lines.toLocaleString()} lines`;
  };

  onMount(() => {
    load();
    api.syntaxNames().then((n) => (syntaxNames = n));
    const media = window.matchMedia("(prefers-color-scheme: light)");
    const onTheme = () => resetRows();
    media.addEventListener("change", onTheme);
    // The file changed on disk (or was saved from another tab).
    const unlisten = listen<PathsChanged>("paths-changed", (e) => {
      const p = e.payload;
      // Its own saves keep the undo history.
      if (p.session !== session.id || !info || p.origin === info.id) return;
      const hit = p.full || p.paths.some((q) => q === path || q === "" || path.startsWith(q + "/"));
      if (!hit || busy) return;
      if (anyDirty) changedOnDisk = true;
      else load(true);
    });
    return () => {
      media.removeEventListener("change", onTheme);
      unlisten.then((f) => f());
    };
  });

  onDestroy(() => {
    // A load still on its way closes its diff when it arrives.
    destroyed = true;
    if (info) api.closeFileDiff(info.id);
  });
</script>

<svelte:window onkeydown={onWindowKey} />

<div class="view" bind:this={root}>
  <div class="toolbar">
    {#if nav}<Nav {nav} />{/if}
    {#if onSwap}<button onclick={onSwap} title="Swap left and right">⇄ Swap</button>{/if}
    {#if nav || onSwap}<span class="sep"></span>{/if}
    {#if info && info.views.length > 1}
      <select bind:value={view} onchange={() => { syntaxOverride = ""; load(); }} disabled={anyDirty} title={anyDirty ? "Save or undo the edits to change the view" : "How the files are shown"}>
        {#each info.views as v (v.id)}
          <option value={v.id}>{v.label}</option>
        {/each}
      </select>
      <span class="sep"></span>
    {/if}
    {#if info?.views.some((v) => v.id === "flatbufferData")}
      <Dropdown label="Schemas">
        <button disabled={anyDirty || loading} onclick={() => pickFlatbufferSchema(null)}>Schema for both files…</button>
        <button disabled={anyDirty || loading} title={flatbufferSchemas[0] ?? "Automatic: same-name sidecar in the left source"} onclick={() => pickFlatbufferSchema(0)}>Left schema…</button>
        <button disabled={anyDirty || loading} title={flatbufferSchemas[1] ?? "Automatic: same-name sidecar in the right source"} onclick={() => pickFlatbufferSchema(1)}>Right schema…</button>
        <button disabled={anyDirty || loading} onclick={() => { flatbufferSchemas = [null, null]; view = null; load(); }}>Use automatic schemas</button>
        <p class="dim">Left: {flatbufferSchemas[0] ?? "Automatic (.bfbs / .fbs sidecar)"}</p>
        <p class="dim">Right: {flatbufferSchemas[1] ?? "Automatic (.bfbs / .fbs sidecar)"}</p>
      </Dropdown>
    {/if}
{#if info?.view !== "image"}
    <button onclick={() => next(-1)} disabled={!projectedSections.length} title="Previous difference (F7)">◀ Prev</button>
    <button onclick={() => next(1)} disabled={!projectedSections.length} title="Next difference (F8)">Next ▶</button>
    {#if info}
      <span class="count" title={shownSection >= 0 || !projectedSections.length ? undefined : "No difference on screen; the view is between these two"}
        >{shownSection >= 0
          ? shownOrdinal + 1
          : viewSections.above === 0
            ? "–"
            : viewSections.above < projectedSections.length
              ? `${viewSections.above}–${viewSections.above + 1}`
              : `${viewSections.above}›`} / {projectedSections.length}</span
      >
    {/if}
    {#if editable[0] || editable[1]}
      <span class="sep"></span>
      <button onclick={() => copySection(currentSection, 0)} disabled={!editable[0] || currentSection < 0} title="Copy the current difference to the left side (Alt+←)">Copy ←</button>
      <button onclick={() => copySection(currentSection, 1)} disabled={!editable[1] || currentSection < 0} title="Copy the current difference to the right side (Alt+→)">Copy →</button>
      <button onclick={undo} disabled={!canUndo} title="Undo (Ctrl+Z)">↶</button>
      <button onclick={redo} disabled={!canRedo} title="Redo (Ctrl+Y)">↷</button>
    {/if}
    <span class="sep"></span>
    <button class:on={findOpen} onclick={() => (findOpen ? closeFind() : openFind())} disabled={!info} title="Find in both files (Ctrl+F)">Find</button>
    <span class="grow"></span>
    <Dropdown label="Options" title="Whitespace, case, diff algorithm, syntax highlighting" changed={optionsChanged}>
      <div class="field">
        <span>Whitespace</span>
        <select bind:value={options.whitespace} onchange={settingsChanged}>
          <option value="exact">Exact</option>
          <option value="normalize">Ignore changes in whitespace</option>
          <option value="ignoreAll">Ignore all whitespace</option>
        </select>
      </div>
      <label><input type="checkbox" bind:checked={options.ignoreCase} onchange={settingsChanged} /> Ignore case</label>
      <label title="Can read better for moved code; slower on files with many repeated lines">
        <input type="checkbox" bind:checked={options.histogram} onchange={settingsChanged} /> Histogram diff instead of Myers
      </label>
      {#if hasSyntax}
        <div class="field">
          <span>Syntax highlighting</span>
          <select bind:value={syntaxOverride} onchange={settingsChanged}>
            <option value="">Automatic{info?.syntax && !syntaxOverride ? ` (${info.syntax})` : ""}</option>
            {#each syntaxNames as name (name)}
              <option value={name}>{name}</option>
            {/each}
          </select>
        </div>
      {/if}
    </Dropdown>
    {/if}
  </div>

  {#if info && info.view !== "image"}
    <FilterBar items={rowFilters} value={rowFilter} onSelect={(id) => setRowFilter(id as RowFilter)} disabled={loading || awaiting}
      hint={rowFilter !== "all" ? "Editing and range selections show all rows" : ""} />
    {#if rowFilter !== "all"}
      <div class="filter-scope">
        <strong>{rowFilters.find((f) => f.id === rowFilter)?.label}</strong>
        <span>· {displayTotal.toLocaleString()} matching rows · Original line numbers</span>
        <button onclick={() => setRowFilter("all")}>Show all</button>
      </div>
    {/if}
  {/if}

  {#if findOpen && info?.view !== "image"}
    <div class="findbar">
      <input
        class="query"
        class:bad={!!findError}
        bind:this={findInput}
        bind:value={findQuery}
        oninput={searchSoon}
        onkeydown={findKey}
        placeholder="Find in both files"
        title="Searches all rows; revealing a hidden match switches to All rows"
        spellcheck="false"
      />
      <button class="opt" class:on={findOptions.matchCase} onclick={() => toggle("matchCase")} title="Match case">Aa</button>
      <button class="opt" class:on={findOptions.wholeWord} onclick={() => toggle("wholeWord")} title="Whole words"><u>ab</u></button>
      <button class="opt" class:on={findOptions.regex} onclick={() => toggle("regex")} title="Regular expression">.*</button>
      <button onclick={() => nextHit(-1)} disabled={!hits.length} title="Previous match (Shift+Enter, Shift+F3)">▲</button>
      <button onclick={() => nextHit(1)} disabled={!hits.length} title="Next match (Enter, F3)">▼</button>
      <span class="found" class:bad={!!findError || (!!findQuery && !hits.length)}>
        {#if findError}{findError}
        {:else if !findQuery}{""}
        {:else if !hits.length}No matches
        {:else}{hitIndex >= 0 ? (hitIndex + 1).toLocaleString() : "–"} of {hits.length.toLocaleString()}{truncated ? "+" : ""}{/if}
      </span>
      <button class="close" onclick={closeFind} title="Close (Esc)">✕</button>
    </div>
  {/if}

  <div class="pathbars">
    <PathBar bind:this={pathbars[0]} info={session.left} suffix={path} modified={dirty[0]} onSave={() => save([0])} edit={edit?.[0]} title="Left" active={activeSide === 0} />
    <PathBar bind:this={pathbars[1]} info={session.right} suffix={path} modified={dirty[1]} onSave={() => save([1])} edit={edit?.[1]} title="Right" active={activeSide === 1} />
  </div>
  {#if info}
    <div class="meta">
      {#each [info.left, info.right] as side, i (i)}
        <span class="metacell">
          <span class="metatext">{eolLabel(side)}</span>
          {#if edit?.[i]}<VersionStrip edit={edit[i]} refreshKey="{(i ? session.right : session.left).display}\n{(i ? session.right : session.left).rev ?? ''}\n{path}" />{/if}
        </span>
      {/each}
    </div>
  {/if}

  {#if eolFix && (eolFix.left || eolFix.right)}
    <div class="banner">
      Line endings differ ({info?.left.lineEnding === "None" ? "none" : info?.left.lineEnding} vs {info?.right.lineEnding === "None" ? "none" : info?.right.lineEnding}).
      {#if eolFix.left}<button onclick={() => setLineEndings(0, eolFix.left!)}>Use {eolFix.left} on the left</button>{/if}
      {#if eolFix.right}<button onclick={() => setLineEndings(1, eolFix.right!)}>Use {eolFix.right} on the right</button>{/if}
    </div>
  {/if}
  {#snippet lineEditor(index: number, s: 0 | 1, fill: boolean, number: string)}
    <div class="ln" class:fill>{number}</div>
    <div class="code editing">
      <input
        class="edit"
        bind:this={input}
        bind:value={draft}
        onkeydown={editKey}
        onpaste={onPaste}
        readonly={awaiting}
        onblur={() => !awaiting && editing?.row === index && editing.side === s && commit(draft, null)}
        spellcheck="false"
      />
    </div>
  {/snippet}

  {#if changedOnDisk}
    <div class="banner warn">
      The file changed on disk. <button onclick={reloadFromDisk}>Reload</button> discards your unsaved edits.
    </div>
  {/if}
  {#if error}
    <div class="banner err">{error}</div>
  {:else if info?.note}
    <div class="banner">{info.note}</div>
  {:else if info?.identical && info.view !== "image"}
    <div class="banner ok">The files are identical.</div>
  {:else if info && info.view !== "image" && !info.sections.length && !loading}
    <div class="banner ok">
      No differences in this view{info.view !== "text" ? " — the files differ only in bytes this view does not show" : ""}{info.stats.minor ? ` (${info.stats.minor} ${info.stats.minor === 1 ? "line differs" : "lines differ"} only in ignored whitespace or case)` : ""}.
    </div>
  {/if}

  {#if info?.view === "image" && info.image}
    <ImagePanel image={info.image} />
  {:else}
  <div class="body">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="overview" onclick={overviewClick} title="Overview: click to jump">
      {#if info && displayTotal}
        {#each sectionMarks as m, i (i)}
          <i style:top="{m.top * 100}%" style:height="max(2px, {m.height * 100}%)"></i>
        {/each}
        {#each hitMarks as m (m)}
          <em style:top="{m * 100}%"></em>
        {/each}
        <b style:top="{((vTop / ROW) / displayTotal) * 100}%" style:height="{Math.min(100, (viewHeight / ROW / displayTotal) * 100)}%"></b>
      {/if}
    </div>
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <div
      class="viewport"
      bind:this={viewport}
      bind:clientHeight={viewHeight}
      onscroll={(e) => (scrollTop = e.currentTarget.scrollTop)}
      onwheel={onWheel}
      onkeydown={onKey}
      onpaste={onViewPaste}
      tabindex="0"
      role="grid"
    >
      {#if info && !info.rows && info.view === "text" && !loading && (editable[0] || editable[1])}
        <!-- Two empty files have no line to click: one row to start typing in. -->
        <div class="row" role="row">
          {#each [0, 1] as const as s (s)}
            {#if s === 1}<div class="mid"></div>{/if}
            {#if editing && editing.row === 0 && editing.side === s}
              {@render lineEditor(0, s, true, "+")}
            {:else}
              <div class="ln fill"></div>
              <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
              <div class="code fill" class:editable={editable[s]} onclick={() => openEditor(s, 0)}>{#if editable[s]}<span class="empty-doc">The file is empty. Click to type.</span>{/if}</div>
            {/if}
          {/each}
        </div>
      {/if}
      {#if info && !displayTotal && rowFilter !== "all" && !loading}
        <div class="filter-empty">No rows match this filter. <button onclick={() => setRowFilter("all")}>Show all rows</button></div>
      {/if}
      <div class="spacer" style:height="{spacerHeight(displayTotal * ROW)}px">
        <div class="window" style:transform="translateY({windowOffset(scrollTop, scale, first, ROW)}px)">
          {#each visible as { index, row } (index)}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div class="row {row?.k ?? 'equal'}" class:loading={!row} class:cur={index === selected} role="row" tabindex="-1" oncontextmenu={(e) => rowContext(e, index)} onmousedown={(e) => e.shiftKey && e.preventDefault()} onclick={(e) => { if (e.shiftKey) showAllRows(); anchor = e.shiftKey && selected !== null ? (anchor ?? selected) : null; selected = index; keepFocus(); }}>
              {#each [row?.l, row?.r] as line, side (side)}
                {@const s = side as 0 | 1}
                {#if side === 1}
                  <div class="mid">
                    {#if sectionStarts.has(index) && (editable[0] || editable[1])}
                      {@const i = sectionStarts.get(index)!}
                      {#if editable[0]}<button class="arrow" onclick={(e) => { e.stopPropagation(); copySection(i, 0); }} title="Copy this difference to the left">←</button>{/if}
                      {#if editable[1]}<button class="arrow" onclick={(e) => { e.stopPropagation(); copySection(i, 1); }} title="Copy this difference to the right">→</button>{/if}
                    {:else}
                      {row && row.k !== "equal" ? { minor: "≈", changed: "≠", removed: "−", added: "+" }[row.k] : ""}
                    {/if}
                  </div>
                {/if}
                {#if editing && editing.row === index && editing.side === s}
                  {@render lineEditor(index, s, !line, line ? String(line.n + 1) : "+")}
                {:else if line}
                  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                  <div class="ln" data-side={SIDE_NAMES[s]} onclick={() => (activeSide = s)}>{line.n + 1}</div>
                  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                  <div class="code" data-side={SIDE_NAMES[s]} class:editable={editable[s]} class:sel={anchor !== null && s === activeSide && lineRange && index >= lineRange[0] && index < lineRange[1]} onclick={(e) => clickCode(e, s, index)}><span class="text" style:transform="translateX({-hscroll}px)">{#if line.o}<span class="cut lead" title="The start of the line is left out, so its first change shows">{line.o.toLocaleString()} characters …</span>{/if}{#each segments(line, hitsAt.get(index * 2 + s)) as seg, si (si)}<span class:mk={seg.mark} class:hit={seg.hit} class:now={seg.now} style:color={seg.now ? null : seg.color}>{seg.text}</span>{/each}</span>{#if line.nn}<span class="cut" title="The other file ends with a line break">no line break at end</span>{/if}{#if line.x}<span class="cut" title="The line is cut for display">… {line.x.toLocaleString()} more characters</span>{/if}</div>
                {:else if row}
                  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                  <div class="ln fill" data-side={SIDE_NAMES[s]} onclick={() => (activeSide = s)}></div>
                  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                  <div class="code fill" data-side={SIDE_NAMES[s]} class:sel={anchor !== null && s === activeSide && lineRange && index >= lineRange[0] && index < lineRange[1]} onclick={(e) => clickCode(e, s, index)}></div>
                {:else}
                  <div class="ln"></div>
                  <div class="code"></div>
                {/if}
              {/each}
            </div>
          {/each}
        </div>
      </div>
    </div>
  </div>

  {/if}

  <div class="detail mono" bind:this={detail} hidden={info?.view === "image"}>
    {#each [["Left", selectedRow?.l], ["Right", selectedRow?.r]] as const as [label, line] (label)}
      <div class="dline">
        <span class="lbl">{label}</span>
        {#if line}{#each segments(line) as seg, si (si)}<span class:mk={seg.mark} style:color={seg.color}>{seg.text}</span>{/each}{/if}
      </div>
    {/each}
  </div>

  {#if menu}
    <Menu items={menu.items} context={menu.context} x={menu.x} y={menu.y} returnFocus={viewport} onClose={() => (menu = null)} />
  {/if}

  <div class="status">
    {#if loading}
      <span>Comparing…</span>
    {:else if info?.view === "image"}
      <span>{info.views.find((v) => v.id === info?.view)?.label}</span>
    {:else if info}
      <span>Showing <b>{displayTotal.toLocaleString()}</b> of <b>{total.toLocaleString()}</b> rows</span>
      <span><b>{projectedSections.length.toLocaleString()}</b> of {info.sections.length.toLocaleString()} difference sections</span>
      {#if selected !== null}<span>Row {selected + 1} of {total}</span>{/if}
      <span>{info.view === "text" ? (info.syntax ?? "Plain text") : info.views.find((v) => v.id === info?.view)?.label}</span>
      {#if busy}<span>{busy}</span>{/if}
      {#if uncommitted}
        <span class="nudge">{uncommitted.name} has uncommitted changes. <button class="link" onclick={compareWithHead} title="Compare it with its last commit (Alt+H)">Compare with HEAD</button> <button class="link dim" onclick={() => { uncommitted = null; uncommittedDismissed = true; }} aria-label="Dismiss">×</button></span>
      {/if}
      <span class="hint">{editable[0] || editable[1] ? "Click to edit · Shift+click or the line number selects lines · Ctrl+C/X/V, Delete · Alt+←/→: copy difference · " : ""}F7 / F8: previous / next difference · Ctrl+F: find</span>
    {/if}
  </div>
</div>

<style>
  .view {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .nudge { color: var(--fg); }
  .nudge .link { border: none; background: none; padding: 0; color: var(--accent); font-size: inherit; cursor: pointer; }
  .nudge .link.dim { color: var(--dim); margin-left: 4px; }
  .filter-scope { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; padding: 4px 10px; background: var(--panel-2); color: var(--dim); font-size: 11px; border-bottom: 1px solid var(--line); }
  .filter-scope strong { color: var(--accent); font-weight: 600; }
  .filter-scope button, .filter-empty button { font-size: 11px; padding: 0 6px; }
  .filter-empty { padding: 30px; color: var(--dim); text-align: center; }
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: center;
    padding: 6px 8px;
    background: var(--panel);
    border-bottom: 1px solid var(--line);
  }
  .grow {
    flex: 1;
  }
  /* Rows still loading: faint bars where the text will be. */
  .row.loading .code::after {
    content: "";
    display: block;
    height: 7px;
    margin: 6px 0 0 8px;
    width: 45%;
    border-radius: 3px;
    background: var(--line);
    opacity: 0.6;
  }
  .count {
    color: var(--dim);
    min-width: 48px;
    text-align: center;
    font-variant-numeric: tabular-nums;
  }
  .pathbars,
  .meta {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1px;
    background: var(--line);
  }
  .meta {
    border-bottom: 1px solid var(--line);
  }
  .metacell {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    background: var(--panel-2);
    padding: 1px 10px 3px;
    color: var(--dim);
    font-size: 11.5px;
  }
  .metatext {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .banner {
    padding: 6px 12px;
    background: var(--panel);
    border-bottom: 1px solid var(--line);
  }
  .banner.ok {
    color: var(--right-only);
  }
  .banner.err {
    color: var(--error);
  }
  .banner.warn {
    color: var(--similar);
  }
  .body {
    flex: 1;
    display: flex;
    min-height: 0;
  }
  .overview {
    width: 16px;
    flex: none;
    position: relative;
    background: var(--chrome);
    border-right: 1px solid var(--line);
    cursor: pointer;
  }
  .overview i {
    position: absolute;
    left: 3px;
    right: 3px;
    background: var(--diff);
    opacity: 0.85;
    border-radius: 1px;
  }
  .overview b {
    position: absolute;
    left: 0;
    right: 0;
    border: 1px solid var(--dim);
    background: rgba(127, 127, 127, 0.12);
  }
  .viewport {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    position: relative;
    outline: none;
    font: 12.5px/19px var(--mono);
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
    grid-template-columns: 48px minmax(0, 1fr) 36px 48px minmax(0, 1fr);
    height: 19px;
    white-space: pre;
  }
  .empty-doc {
    color: var(--faint);
    font-style: italic;
  }
  .row.cur {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
  }
  .ln {
    color: var(--faint);
    text-align: right;
    padding-right: 8px;
    background: var(--panel-2);
    user-select: none;
  }
  .code {
    overflow: hidden;
    padding-left: 6px;
  }
  .code.sel {
    background: var(--select, #3478f633);
  }
  .code.editable {
    cursor: text;
  }
  .code > .text {
    display: inline-block;
  }
  .code.editing {
    padding: 0;
    background: var(--input);
  }
  .edit {
    width: 100%;
    height: 19px;
    border: none;
    border-radius: 0;
    outline: 1px solid var(--accent);
    outline-offset: -1px;
    padding: 0 0 0 6px;
    font: inherit;
    background: var(--input);
    color: var(--text);
    tab-size: 4;
  }
  .mid {
    display: flex;
    justify-content: center;
    gap: 1px;
    background: var(--panel-2);
    border-left: 1px solid var(--line);
    border-right: 1px solid var(--line);
    text-align: center;
    color: var(--dim);
    font-size: 11px;
  }
  .arrow {
    padding: 0 3px;
    min-width: 0;
    height: 17px;
    margin-top: 1px;
    line-height: 15px;
    font-size: 11px;
    border-radius: 3px;
  }
  .changed .code {
    background: var(--row-changed);
  }
  .changed .ln {
    background: var(--row-changed-gutter);
  }
  .added .code {
    background: var(--row-added);
  }
  .added .ln {
    background: var(--row-added-gutter);
  }
  .removed .code {
    background: var(--row-removed);
  }
  .removed .ln {
    background: var(--row-removed-gutter);
  }
  .minor .code {
    background: var(--row-minor);
  }
  .fill,
  .changed .fill,
  .added .fill,
  .removed .fill {
    background: var(--filler);
  }
  .mk {
    background: var(--mark);
    border-radius: 2px;
  }
  .hit {
    background: var(--hit);
    border-radius: 2px;
  }
  .hit.now {
    background: var(--hit-cur);
    color: var(--hit-cur-text);
    outline: 1px solid var(--hit-cur);
  }
  .findbar {
    display: flex;
    gap: 4px;
    align-items: center;
    padding: 4px 8px;
    background: var(--panel);
    border-bottom: 1px solid var(--line);
  }
  .findbar .query {
    width: min(420px, 50%);
    font-family: var(--mono);
  }
  .findbar .query.bad {
    outline: 1px solid var(--error);
  }
  .findbar .opt {
    min-width: 30px;
    font-family: var(--mono);
    font-size: 11.5px;
  }
  .found {
    color: var(--dim);
    margin-left: 6px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .found.bad {
    color: var(--error);
  }
  .findbar .close {
    margin-left: auto;
  }
  .overview em {
    position: absolute;
    right: 0;
    width: 5px;
    height: 2px;
    background: var(--hit-cur);
  }
  .cut {
    margin-left: 8px;
    padding: 0 6px;
    border-radius: 3px;
    background: var(--panel);
    color: var(--dim);
    font-family: var(--ui);
    font-size: 11px;
  }
  .cut.lead {
    margin-left: 0;
    margin-right: 8px;
  }
  .detail[hidden] {
    display: none;
  }
  .detail {
    border-top: 1px solid var(--line);
    background: var(--panel-2);
    font-size: 12.5px;
    line-height: 19px;
    padding: 2px 0;
    overflow-x: auto;
  }
  .dline {
    white-space: pre;
    padding: 0 10px;
    min-height: 19px;
  }
  .lbl {
    display: inline-block;
    width: 52px;
    color: var(--dim);
    font-family: var(--ui);
    font-size: 11.5px;
  }
  .status {
    display: flex;
    flex-wrap: wrap;
    gap: 16px;
    padding: 4px 10px;
    min-height: 26px;
    align-items: center;
    background: var(--chrome);
    border-top: 1px solid var(--line);
    color: var(--dim);
    font-size: 12px;
  }
  .status b {
    color: var(--text);
  }
  .hint {
    margin-left: auto;
  }
</style>
