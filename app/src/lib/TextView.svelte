<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { ask } from "@tauri-apps/plugin-dialog";
  import {
    api,
    defaultDiffOptions,
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
  import type { Command } from "./commands";
  import Dropdown from "./Dropdown.svelte";
  import Nav, { type NavActions } from "./Nav.svelte";
  import PathBar, { type SideEdit } from "./PathBar.svelte";
  import { scaledWheel, scrollScale, spacerHeight, windowOffset } from "./vscroll";
  import { formatSize } from "./format";

  let {
    session,
    path,
    onDirty,
    onSwap,
    edit,
    nav,
  }: {
    session: SessionInfo;
    path: string;
    onDirty?: (dirty: boolean) => void;
    onSwap?: () => void;
    edit?: [SideEdit, SideEdit];
    nav?: NavActions;
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
  let section = $state(-1);

  // Editing state.
  let dirty = $state<[boolean, boolean]>([false, false]);
  let canUndo = $state(false);
  let canRedo = $state(false);
  /** The side that keyboard commands (Enter, copy) act on: the last one clicked. */
  let activeSide = $state<0 | 1>(1);
  /** The line being typed in: its row, and its line number (null in a filler row). */
  let editing = $state<{ side: 0 | 1; row: number; line: number | null } | null>(null);
  let draft = $state("");
  let original = "";
  let input = $state<HTMLInputElement>();
  /** After an edit, the caret waits for its row to be loaded. */
  let caretWanted: { side: 0 | 1; line: number; col: number; hint: number } | null = null;
  /** File changed on disk while it has unsaved edits. */
  let changedOnDisk = $state(false);
  // Edits run one after another, in the order they were made.
  let queue: Promise<unknown> = Promise.resolve();

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

  const total = $derived(info?.rows ?? 0);
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
    if (!total) return [];
    const buckets = new Set<number>();
    for (const h of hits) buckets.add(Math.floor((h.row / total) * 600));
    return [...buckets].map((b) => b / 600);
  });
  // Very long files scroll through a scaled spacer (see vscroll.ts); `vTop`
  // is the scroll position in row pixels.
  const scale = $derived(scrollScale(total * ROW, viewHeight));
  const vTop = $derived(scrollTop * scale);
  const first = $derived(Math.max(0, Math.floor(vTop / ROW) - 20));
  const last = $derived(Math.min(total, first + Math.ceil(viewHeight / ROW) + 40));
  const visible = $derived.by(() => {
    void cacheVersion;
    const out: { index: number; row: RowView | undefined }[] = [];
    for (let i = first; i < last; i++) out.push({ index: i, row: cache.get(i) });
    return out;
  });
  const selectedRow = $derived.by(() => {
    void cacheVersion;
    return selected === null ? undefined : cache.get(selected);
  });
  const editable = $derived<[boolean, boolean]>([!!info?.left.editable, !!info?.right.editable]);
  const anyDirty = $derived(dirty[0] || dirty[1]);
  /** Section index by its first row, for the copy arrows. */
  /**
   * The overview's marks: sections merged per thousandth of the file, so a file
   * with 100 000 differences draws at most 1 000 marks.
   */
  const sectionMarks = $derived.by(() => {
    const marks: { top: number; height: number }[] = [];
    if (!info || !total) return marks;
    const B = 1000;
    let from = -1;
    let to = -1;
    for (const s of info.sections) {
      const a = Math.floor((s.start / total) * B);
      const b = Math.max(a + 1, Math.ceil((s.end / total) * B));
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
  const sectionStarts = $derived(new Map((info?.sections ?? []).map((s, i) => [s.start, i])));
  /** The section the toolbar's copy buttons act on. */
  const currentSection = $derived.by(() => {
    if (!info) return -1;
    if (selected !== null) {
      const i = info.sections.findIndex((s) => s.start <= selected! && selected! < s.end);
      if (i >= 0) return i;
    }
    return section;
  });

  $effect(() => onDirty?.(anyDirty));

  async function load(keepPosition = false) {
    loading = true;
    error = "";
    changedOnDisk = false;
    const previous = info?.id;
    const top = viewport?.scrollTop ?? 0;
    try {
      info = await api.openFileDiff(session.id, path, $state.snapshot(options), syntaxOverride || null, view);
      view = info.view;
      dirty = [false, false];
      canUndo = canRedo = false;
      editing = null;
      resetRows();
      section = -1;
      if (previous !== undefined) api.closeFileDiff(previous);
      await tick();
      if (keepPosition && viewport) viewport.scrollTop = top;
      else if (info.sections.length) goTo(0);
      else ensureLoaded();
      if (findOpen && findQuery) runSearch(keepPosition);
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  function resetRows() {
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
    const here = Math.floor(first / CHUNK);
    const far = [...loaded].sort((a, b) => Math.abs(b - here) - Math.abs(a - here));
    for (const chunk of far.slice(0, loaded.size - KEEP_CHUNKS)) {
      loaded.delete(chunk);
      pending.delete(chunk);
      for (let i = chunk * CHUNK; i < (chunk + 1) * CHUNK; i++) cache.delete(i);
    }
  }

  /** The chunks to load, most needed first: those on screen, then their neighbours. */
  function wantedChunks(): number[] {
    const from = Math.floor(first / CHUNK);
    const to = Math.floor(Math.max(first, last - 1) / CHUNK);
    const list: number[] = [];
    if (caretWanted) list.push(Math.floor(caretWanted.hint / CHUNK));
    for (let c = from; c <= to; c++) list.push(c);
    // So that scrolling on finds its rows loaded already.
    if ((to + 1) * CHUNK < total) list.push(to + 1);
    if (from > 0) list.push(from - 1);
    return list;
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
    if (!info || !viewport || !info.sections.length) return;
    section = Math.max(0, Math.min(info.sections.length - 1, i));
    const start = info.sections[section].start;
    selected = start;
    scrollToV((start - 3) * ROW);
  }

  function next(dir: 1 | -1) {
    if (!info?.sections.length) return;
    if (section === -1) {
      const top = Math.floor(vTop / ROW);
      const idx = info.sections.findIndex((s) => s.start > top);
      goTo(dir === 1 ? (idx === -1 ? info.sections.length - 1 : idx) : Math.max(0, (idx === -1 ? info.sections.length : idx) - 1));
    } else {
      goTo(section + dir);
    }
  }

  /** Scrolls a row into view if it is not visible. */
  function reveal(row: number) {
    if (!viewport) return;
    const top = row * ROW;
    if (top < vTop || top + ROW > vTop + viewHeight) scrollToV(top - viewHeight / 3);
  }

  // ---- Editing ----

  function enqueue<T>(task: () => Promise<T>): Promise<T | undefined> {
    const run = queue.then(task).catch((e) => {
      error = String(e);
      return undefined;
    });
    queue = run;
    return run;
  }

  function applyEdit(e: EditInfo | undefined) {
    if (!e || !info) return;
    info = {
      ...info,
      rows: e.rows,
      sections: e.sections,
      stats: e.stats,
      identical: e.identical,
      note: null,
      left: { ...info.left, lines: e.lines[0], exists: e.exists[0] },
      right: { ...info.right, lines: e.lines[1], exists: e.exists[1] },
    };
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
      editing = null;
      reveal(e.caret.row);
    }
    resetRows();
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
          return;
        }
      }
    }
  }

  function lineOf(side: 0 | 1, row: RowView | undefined): LineView | null | undefined {
    return row && (side === 0 ? row.l : row.r);
  }

  function openEditor(side: 0 | 1, index: number, col?: number) {
    const row = cache.get(index);
    if (!row || !editable[side]) return;
    const line = lineOf(side, row);
    if (line?.x) {
      error = "This line is too long to edit here.";
      return;
    }
    activeSide = side;
    selected = index;
    editing = { side, row: index, line: line ? line.n : null };
    draft = original = line?.t ?? "";
    const at = Math.min(col ?? draft.length, draft.length);
    tick().then(() => {
      input?.focus();
      input?.setSelectionRange(at, at);
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

  function startEdit(e: MouseEvent, side: 0 | 1, index: number) {
    if (!editable[side]) return;
    openEditor(side, index, columnAt(e, e.currentTarget as HTMLElement));
  }

  /** Sends the typed line; `text` may hold line breaks, `caret` where typing goes on. */
  function commit(text: string, caret: number | null, then?: (e: EditInfo | undefined) => void) {
    const ed = editing;
    if (!ed || !info) return;
    const id = info.id;
    const side = SIDE_NAMES[ed.side];
    if (caret === null) editing = null;
    if (text === original && caret === null) return;
    enqueue(() =>
      ed.line === null
        ? api.insertLine(id, side, ed.row, text)
        : api.editText(id, side, { line: ed.line, col: 0 }, { line: ed.line, col: END }, text, caret),
    ).then((e) => {
      applyEdit(e);
      then?.(e);
    });
  }

  function editKey(e: KeyboardEvent) {
    const ed = editing;
    if (!ed || !info || !input) return;
    const [a, b] = [input.selectionStart ?? 0, input.selectionEnd ?? 0];
    const id = info.id;
    const side = SIDE_NAMES[ed.side];
    const mod = e.ctrlKey || e.metaKey;
    if (e.key === "Enter") {
      commit(draft.slice(0, a) + "\n" + draft.slice(b), a + 1);
    } else if (e.key === "Escape") {
      editing = null;
      viewport?.focus();
    } else if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      const target = ed.line === null ? null : ed.line + (e.key === "ArrowUp" ? -1 : 1);
      const col = a;
      const sideLines = ed.side === 0 ? info.left.lines : info.right.lines;
      if (target === null || target < 0 || target >= sideLines) return;
      const move = (hint: number) => {
        caretWanted = { side: ed.side, line: target, col, hint: hint + (e.key === "ArrowUp" ? -1 : 1) };
        editing = null;
        placeCaret();
      };
      if (draft === original) move(ed.row);
      else commit(draft, col, (r) => r?.caret && move(r.caret.row));
    } else if (e.key === "Backspace" && a === 0 && b === 0 && ed.line !== null && ed.line > 0) {
      // Joins the line to the one above.
      const line = ed.line;
      editing = null;
      enqueue(() => api.editText(id, side, { line: line - 1, col: END }, { line, col: END }, draft, 0)).then((r) => {
        if (r?.caret) editing = { side: ed.side, row: r.caret.row, line: r.caret.line };
        applyEdit(r);
      });
    } else if (e.key === "Delete" && a === draft.length && b === a && ed.line !== null) {
      // Joins the line below to this one.
      const line = ed.line;
      const sideLines = ed.side === 0 ? info.left.lines : info.right.lines;
      if (line + 1 >= sideLines) return;
      editing = null;
      enqueue(() => api.editText(id, side, { line, col: 0 }, { line: line + 1, col: 0 }, draft, a)).then((r) => {
        if (r?.caret) editing = { side: ed.side, row: r.caret.row, line: r.caret.line };
        applyEdit(r);
      });
    } else if (e.key === "Tab" && !mod) {
      draft = draft.slice(0, a) + "\t" + draft.slice(b);
      tick().then(() => input?.setSelectionRange(a + 1, a + 1));
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

  function onPaste(e: ClipboardEvent) {
    const text = e.clipboardData?.getData("text/plain") ?? "";
    if (!input || !/[\r\n]/.test(text)) return;
    // Several lines: they replace the selection and become lines of the file.
    e.preventDefault();
    const [a, b] = [input.selectionStart ?? 0, input.selectionEnd ?? 0];
    commit(draft.slice(0, a) + text + draft.slice(b), a + text.length);
  }

  function copySection(i: number, to: 0 | 1) {
    if (!info || i < 0 || !editable[to]) return;
    const s = info.sections[i];
    const id = info.id;
    editing = null;
    enqueue(() => api.copyRows(id, s.start, s.end, SIDE_NAMES[to])).then(applyEdit);
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
    // Typed edits land first, so they are saved too.
    await queue;
    for (const side of sides) {
      if (!dirty[side]) continue;
      busy = "Saving…";
      try {
        applyEdit(await api.saveSide(id, SIDE_NAMES[side]));
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
    info?.view === "text" || info?.view === "prettyXml" || info?.view === "prettyJson" || info?.view === "chatTemplate",
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
      { group, title: "Previous difference", keys: "F7", enabled: text && !!info?.sections.length, run: () => next(-1) },
      { group, title: "Next difference", keys: "F8", enabled: text && !!info?.sections.length, run: () => next(1) },
      { group, title: "Find in both files", keys: "Ctrl+F", enabled: text, run: openFind },
    ];
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
    enqueue(() => api.updateDiff(id, $state.snapshot(options), syntaxOverride || null)).then(applyEdit);
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
    hitIndex = i;
    selected = h.row;
    activeSide = h.side;
    section = -1;
    if (viewport) {
      const top = h.row * ROW;
      if (top < vTop || top + ROW > vTop + viewHeight) scrollToV(top - viewHeight / 3);
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
    tick().then(revealHitSideways);
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
    else return;
    e.preventDefault();
  }

  // ---- Keys, mouse ----

  /**
   * Keeps keyboard focus on the scroll area: a clicked row would take it, and lose
   * it for good once scrolling takes that row out of the page.
   */
  function keepFocus() {
    viewport?.focus({ preventScroll: true });
  }

  function onKey(e: KeyboardEvent) {
    // Keys typed into the line editor.
    if (e.target instanceof HTMLInputElement) return;
    const mod = e.ctrlKey || e.metaKey;
    if (e.key === "F8" || (e.altKey && e.key === "ArrowDown") || e.key === "n") next(1);
    else if (e.key === "F7" || (e.altKey && e.key === "ArrowUp") || e.key === "p") next(-1);
    else if (e.altKey && e.key === "ArrowRight") copySection(currentSection, 1);
    else if (e.altKey && e.key === "ArrowLeft") copySection(currentSection, 0);
    else if (mod && e.key === "z" && !e.shiftKey) undo();
    else if (mod && (e.key === "y" || (e.key === "Z" && e.shiftKey) || (e.key === "z" && e.shiftKey))) redo();
    else if (mod && e.key === "s") save();
    else if ((e.key === "Enter" || e.key === "F2") && selected !== null) openEditor(activeSide, selected);
    else if (e.key === "ArrowDown" && selected !== null) {
      selected = Math.min(total - 1, selected + 1);
      reveal(selected);
    } else if (e.key === "ArrowUp" && selected !== null) {
      selected = Math.max(0, selected - 1);
      reveal(selected);
    } else if (e.key === "PageDown" || e.key === "PageUp") {
      const page = Math.max(1, Math.floor(viewHeight / ROW) - 2) * (e.key === "PageDown" ? 1 : -1);
      if (selected !== null) selected = Math.max(0, Math.min(total - 1, selected + page));
      scrollToV(vTop + page * ROW);
    } else if (e.key === "Home" || e.key === "End") {
      const end = e.key === "End";
      if (selected !== null) selected = end ? total - 1 : 0;
      scrollToV(end ? total * ROW : 0);
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
    if (!viewport || !total) return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const row = Math.floor(((e.clientY - rect.top) / rect.height) * total);
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
  function segments(line: LineView, found: Hit[] = []): Segment[] {
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
        now: !!currentHit && found.some((h) => h === currentHit && h.start <= a && b <= h.end),
      });
    }
    return out;
  }

  const eolLabel = (s: FileDiffInfo["left"]) => {
    if (!s.exists) return "missing";
    if (info?.view === "image") return formatSize(s.size);
    const kind = info?.view === "text" ? `${s.encoding}${s.bom ? " BOM" : ""} · ${s.lineEnding}` : s.encoding;
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
{#if info?.view !== "image"}
    <button onclick={() => next(-1)} disabled={!info?.sections.length} title="Previous difference (F7)">◀ Prev</button>
    <button onclick={() => next(1)} disabled={!info?.sections.length} title="Next difference (F8)">Next ▶</button>
    {#if info}
      <span class="count">{section >= 0 ? section + 1 : "–"} / {info.sections.length}</span>
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
    <PathBar bind:this={pathbars[0]} info={session.left} suffix={path} modified={dirty[0]} onSave={() => save([0])} edit={edit?.[0]} title="Left" />
    <PathBar bind:this={pathbars[1]} info={session.right} suffix={path} modified={dirty[1]} onSave={() => save([1])} edit={edit?.[1]} title="Right" />
  </div>
  {#if info}
    <div class="meta">
      <span>{eolLabel(info.left)}</span>
      <span>{eolLabel(info.right)}</span>
    </div>
  {/if}

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
      {#if info && total}
        {#each sectionMarks as m, i (i)}
          <i style:top="{m.top * 100}%" style:height="max(2px, {m.height * 100}%)"></i>
        {/each}
        {#each hitMarks as m (m)}
          <em style:top="{m * 100}%"></em>
        {/each}
        <b style:top="{((vTop / ROW) / total) * 100}%" style:height="{Math.min(100, (viewHeight / ROW / total) * 100)}%"></b>
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
      tabindex="0"
      role="grid"
    >
      <div class="spacer" style:height="{spacerHeight(total * ROW)}px">
        <div class="window" style:transform="translateY({windowOffset(scrollTop, scale, first, ROW)}px)">
          {#each visible as { index, row } (index)}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div class="row {row?.k ?? 'equal'}" class:loading={!row} class:cur={index === selected} role="row" tabindex="-1" onclick={() => { selected = index; keepFocus(); }}>
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
                  <div class="ln" class:fill={!line}>{line ? line.n + 1 : "+"}</div>
                  <div class="code editing">
                    <input
                      class="edit"
                      bind:this={input}
                      bind:value={draft}
                      onkeydown={editKey}
                      onpaste={onPaste}
                      onblur={() => commit(draft, null)}
                      spellcheck="false"
                    />
                  </div>
                {:else if line}
                  <div class="ln">{line.n + 1}</div>
                  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                  <div class="code" class:editable={editable[s]} onclick={() => (activeSide = s)} ondblclick={(e) => startEdit(e, s, index)}><span class="text" style:transform="translateX({-hscroll}px)">{#each segments(line, hitsAt.get(index * 2 + s)) as seg, si (si)}<span class:mk={seg.mark} class:hit={seg.hit} class:now={seg.now} style:color={seg.now ? null : seg.color}>{seg.text}</span>{/each}</span>{#if line.nn}<span class="cut" title="The other file ends with a line break">no line break at end</span>{/if}{#if line.x}<span class="cut" title="The line is cut for display">… {line.x.toLocaleString()} more characters</span>{/if}</div>
                {:else if row}
                  <div class="ln fill"></div>
                  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                  <div class="code fill" onclick={() => (activeSide = s)} ondblclick={(e) => startEdit(e, s, index)}></div>
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

  <div class="detail mono" hidden={info?.view === "image"}>
    {#each [["Left", selectedRow?.l], ["Right", selectedRow?.r]] as const as [label, line] (label)}
      <div class="dline">
        <span class="lbl">{label}</span>
        {#if line}{#each segments(line) as seg, si (si)}<span class:mk={seg.mark} style:color={seg.color}>{seg.text}</span>{/each}{/if}
      </div>
    {/each}
  </div>

  <div class="status">
    {#if loading}
      <span>Comparing…</span>
    {:else if info?.view === "image"}
      <span>{info.views.find((v) => v.id === info?.view)?.label}</span>
    {:else if info}
      <span><b>{info.sections.length}</b> difference sections</span>
      <span class="c-diff">{info.stats.changed} changed</span>
      <span class="c-add">{info.stats.added} added</span>
      <span class="c-rem">{info.stats.removed} removed</span>
      {#if selected !== null}<span>Row {selected + 1} of {total}</span>{/if}
      <span>{info.view === "text" ? (info.syntax ?? "Plain text") : info.views.find((v) => v.id === info?.view)?.label}</span>
      {#if busy}<span>{busy}</span>{/if}
      <span class="hint">{editable[0] || editable[1] ? "Double-click to edit · Alt+←/→: copy difference · " : ""}F7 / F8: previous / next difference · Ctrl+F: find</span>
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
  .meta span {
    background: var(--panel-2);
    padding: 1px 10px 3px;
    color: var(--dim);
    font-size: 11.5px;
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
  .c-diff {
    color: var(--diff);
  }
  .c-add {
    color: var(--right-only);
  }
  .c-rem {
    color: var(--left-only);
  }
  .hint {
    margin-left: auto;
  }
</style>
