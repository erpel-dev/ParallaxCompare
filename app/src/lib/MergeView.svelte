<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import { ask } from "@tauri-apps/plugin-dialog";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { api, END, type Choice, type InputRowView, type LineView, type MergeInfo, type MergeRequest, type ResultRowView } from "./api";
  import { notify } from "./toast.svelte";
  import { RowPane, ROW } from "./rowPane.svelte";
  import { syntaxMerge } from "./syntaxMerge.svelte";
  import { scaledWheel, spacerHeight, windowOffset } from "./vscroll";

  let {
    request,
    onInfo,
    onDirty,
    onSaved,
  }: {
    request: MergeRequest;
    /** The merge was opened or changed. */
    onInfo?: (info: MergeInfo) => void;
    onDirty?: (dirty: boolean) => void;
    /** The result was written; `clean` without conflict markers. */
    onSaved?: (clean: boolean) => void;
  } = $props();

  // Raw: replaced as a whole after every change.
  let info = $state.raw<MergeInfo | null>(null);
  let error = $state("");
  let loading = $state(true);
  let destroyed = false;

  const inputs = new RowPane<InputRowView>((start, count) => (info ? api.mergeInputRows(info.id, start, count) : Promise.resolve([])));
  const results = new RowPane<ResultRowView>((start, count) => (info ? api.mergeResultRows(info.id, start, count) : Promise.resolve([])));

  let topEl = $state<HTMLDivElement>();
  let resultEl = $state<HTMLDivElement>();
  let root = $state<HTMLDivElement>();
  /** Share of the height the inputs take; the result gets the rest. */
  let split = $state(0.5);
  /** The conflict the keyboard acts on (Alt+arrows): its block. */
  let current = $state<number | null>(null);
  let savedNote = $state("");

  // Editing a result line.
  let editing = $state<{ line: number } | null>(null);
  let draft = $state("");
  let original = "";
  let input = $state<HTMLInputElement>();
  // Edits run one after another, in the order they were made.
  let queue: Promise<unknown> = Promise.resolve();

  // The syntax-aware merge (built in, or mergiraf), unless it is turned off.
  let structuredRunning = $state(false);
  let structuredError = $state("");
  let syntaxMenu = $state(false);
  /** The syntax-aware result shown: from the engine chosen now, not one chosen before. */
  const active = $derived(
    syntaxMerge.enabled && !!syntaxMerge.found && !!info?.structured && info.structured.engine === syntaxMerge.found.name,
  );
  /** The syntax-aware solution by block. */
  const suggestionText = $derived(new Map(active ? (info?.suggestions ?? []) : []));
  const conflictOf = $derived(new Map((info?.conflicts ?? []).map((c) => [c.block, c])));
  /** Open conflicts the syntax-aware merge solved completely. */
  const openSuggestions = $derived(
    active ? (info?.conflicts ?? []).filter((c) => c.suggestion && !c.partial && c.choice === "unresolved") : [],
  );

  /** Conflict number (1-based) by block. */
  const conflictNo = $derived(new Map((info?.conflicts ?? []).map((c, i) => [c.block, i + 1])));
  const anyDirty = $derived(!!info?.dirty || (editing !== null && draft !== original));
  $effect(() => onDirty?.(anyDirty));

  onMount(async () => {
    try {
      const opened = await api.openMerge(request);
      if (destroyed) {
        api.closeMerge(opened.id);
        return;
      }
      apply(opened, true);
      runStructured();
      const first = opened.conflicts.find((c) => c.choice === "unresolved") ?? opened.conflicts[0];
      await tick();
      if (first) goTo(first.block);
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  });

  onDestroy(() => {
    destroyed = true;
    if (info) api.closeMerge(info.id);
  });

  function apply(next: MergeInfo, first = false) {
    info = next;
    if (first) inputs.reset(next.inputRows);
    results.reset(next.resultRows);
    onInfo?.(next);
  }

  /** Runs a change in order, and shows its result. */
  function change(run: () => Promise<MergeInfo>): Promise<MergeInfo | undefined> {
    const next = queue.then(run).then(
      (i) => {
        if (!destroyed) apply(i);
        return i;
      },
      (e) => {
        error = String(e);
        return undefined;
      },
    );
    queue = next;
    return next;
  }

  // ---- conflicts ----

  function choose(block: number, choice: Choice) {
    if (!info) return;
    const id = info.id;
    current = block;
    change(() => api.mergeResolve(id, block, choice)).then((i) => {
      // On to the next open conflict, as F8 would.
      if (i && choice !== "auto" && choice !== "unresolved") {
        const next = i.conflicts.find((c) => c.choice === "unresolved" && c.block > block) ?? i.conflicts.find((c) => c.choice === "unresolved");
        if (next) goTo(next.block);
      }
    });
  }

  // ---- syntax-aware merge ----

  /** Asks the syntax-aware merge for solutions, in the background; the line merge stays as it is. */
  async function runStructured(again = false) {
    structuredError = "";
    const found = await syntaxMerge.detect(again);
    if (!found || !info || destroyed) return;
    const id = info.id;
    structuredRunning = true;
    try {
      await api.mergeStructured(id, syntaxMerge.engine, syntaxMerge.program);
      // Read back in order with any edit made meanwhile.
      if (!destroyed) await change(() => api.mergeInfo(id));
    } catch (e) {
      structuredError = String(e);
    } finally {
      structuredRunning = false;
    }
  }

  function applySuggestions() {
    if (!info) return;
    const id = info.id;
    for (const c of openSuggestions) change(() => api.mergeResolve(id, c.block, "syntax"));
  }

  function useStructured() {
    if (!info) return;
    const id = info.id;
    change(() => api.mergeUseStructuredResult(id));
  }

  function settingsChanged() {
    syntaxMerge.save();
    structuredError = "";
    if (syntaxMerge.enabled) runStructured(true);
  }

  function goTo(block: number) {
    if (!info) return;
    current = block;
    const at = info.blockRows[block];
    if (!at) return;
    scrollPane(topEl, inputs, at[0]);
    scrollPane(resultEl, results, at[1]);
  }

  /** Scrolls so that `row` shows a few rows below the top. */
  function scrollPane(el: HTMLDivElement | undefined, pane: RowPane<unknown>, row: number) {
    if (!el) return;
    const target = Math.max(0, row - 3) * ROW;
    syncing = el;
    el.scrollTop = target / pane.scale;
    pane.scrollTop = el.scrollTop;
    pane.ensure();
  }

  function step(forward: boolean) {
    if (!info || !info.conflicts.length) return;
    const list = info.conflicts.some((c) => c.choice === "unresolved") ? info.conflicts.filter((c) => c.choice === "unresolved") : info.conflicts;
    const at = current ?? -1;
    const next = forward ? (list.find((c) => c.block > at) ?? list[0]) : ([...list].reverse().find((c) => c.block < at) ?? list[list.length - 1]);
    goTo(next.block);
  }

  // ---- scrolling the panes together, by block ----

  let syncing: HTMLDivElement | null = null;

  /** The block of a row, and the row's offset in it, by the first rows of the blocks. */
  function blockAt(row: number, which: 0 | 1): [number, number] {
    const rows = info?.blockRows ?? [];
    let lo = 0;
    let hi = rows.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (rows[mid][which] <= row) lo = mid;
      else hi = mid - 1;
    }
    return [lo, row - (rows[lo]?.[which] ?? 0)];
  }

  function onScroll(which: 0 | 1) {
    const [el, pane, other, otherPane] = which === 0 ? [topEl, inputs, resultEl, results] : [resultEl, results, topEl, inputs];
    if (!el) return;
    pane.scrollTop = el.scrollTop;
    pane.ensure();
    if (syncing === el) {
      syncing = null;
      return;
    }
    if (!other || !info) return;
    const row = Math.floor((el.scrollTop * pane.scale) / ROW);
    const [block, offset] = blockAt(row, which);
    const start = info.blockRows[block][1 - which];
    const end = info.blockRows[block + 1]?.[1 - which] ?? otherPane.total;
    const target = (start + Math.min(offset, Math.max(0, end - start - 1))) * ROW + (((el.scrollTop * pane.scale) % ROW) as number);
    syncing = other;
    other.scrollTop = target / otherPane.scale;
    otherPane.scrollTop = other.scrollTop;
    otherPane.ensure();
  }

  function onWheel(e: WheelEvent, el: HTMLDivElement | undefined, pane: RowPane<unknown>) {
    if (el && scaledWheel(el, pane.scale, e, ROW)) e.preventDefault();
  }

  // ---- editing the result ----

  function lineText(line: LineView): string {
    return line.t;
  }

  async function openEditor(row: ResultRowView, col: number | null = null) {
    if (!info || !row.l) return;
    if (editing) await commit();
    // Very long lines are shown cut; edit the whole line.
    const text = row.l.x || row.l.o ? (await api.mergeResultText(info.id, row.l.n, row.l.n + 1)).replace(/\r?\n$|\r$/, "") : lineText(row.l);
    editing = { line: row.l.n };
    draft = original = text;
    current = row.k === "conflict" ? row.i : current;
    await tick();
    input?.focus();
    const at = col ?? text.length;
    input?.setSelectionRange(at, at);
  }

  /** Writes the line being edited; `then` runs with the caret's next line once queued. */
  function commit(): Promise<unknown> {
    if (!editing || !info) return Promise.resolve();
    const { line } = editing;
    const text = draft;
    editing = null;
    if (text === original) return Promise.resolve();
    const id = info.id;
    return change(() => api.mergeEdit(id, { line, col: 0 }, { line, col: END }, text));
  }

  async function editKey(e: KeyboardEvent) {
    if (!editing || !info || !input) return;
    const id = info.id;
    const line = editing.line;
    const [a, b] = [input.selectionStart ?? 0, input.selectionEnd ?? 0];
    if (e.key === "Escape") {
      e.preventDefault();
      draft = original;
      editing = null;
      root?.focus();
    } else if (e.key === "Enter") {
      // Splits the line; typing goes on in the new line at once.
      e.preventDefault();
      const head = draft.slice(0, a);
      const tail = draft.slice(b);
      editing = null;
      change(() => api.mergeEdit(id, { line, col: 0 }, { line, col: END }, head + "\n" + tail));
      editing = { line: line + 1 };
      draft = original = tail;
      await tick();
      input?.focus();
      input?.setSelectionRange(0, 0);
    } else if ((e.key === "ArrowUp" || e.key === "ArrowDown") && !e.shiftKey) {
      e.preventDefault();
      const target = line + (e.key === "ArrowUp" ? -1 : 1);
      const col = a;
      await commit();
      await queue;
      const row = findResultRow(target);
      if (row) openEditor(row, col);
      else root?.focus();
    } else if (e.key === "Backspace" && a === 0 && b === 0 && line > 0) {
      // Joins the line to the one above.
      e.preventDefault();
      const above = findResultRow(line - 1);
      if (!above?.l) return;
      const prev = above.l.x || above.l.o ? null : above.l.t;
      const text = draft;
      editing = null;
      await change(() => api.mergeEdit(id, { line: line - 1, col: END }, { line, col: END }, text));
      const row = findResultRow(line - 1) ?? (await waitRow(line - 1));
      if (row) openEditor(row, prev?.length ?? null);
    } else if (e.key === "Delete" && a === draft.length && b === a && info && line + 1 < info.resultRows) {
      e.preventDefault();
      const text = draft;
      editing = null;
      await change(() => api.mergeEdit(id, { line, col: 0 }, { line: line + 1, col: 0 }, text));
      const row = findResultRow(line) ?? (await waitRow(line));
      if (row) openEditor(row, text.length);
    } else if (e.key === "Tab") {
      e.preventDefault();
      draft = draft.slice(0, a) + "\t" + draft.slice(b);
      await tick();
      input.setSelectionRange(a + 1, a + 1);
    } else if ((e.ctrlKey || e.metaKey) && e.key === "s") {
      e.preventDefault();
      await commit();
      save();
    }
  }

  function onPaste(e: ClipboardEvent) {
    const text = e.clipboardData?.getData("text/plain") ?? "";
    if (!editing || !info || !input || !/[\r\n]/.test(text)) return;
    // Several lines become lines of the result.
    e.preventDefault();
    const [a, b] = [input.selectionStart ?? 0, input.selectionEnd ?? 0];
    const line = editing.line;
    const merged = draft.slice(0, a) + text + draft.slice(b);
    const id = info.id;
    editing = null;
    change(() => api.mergeEdit(id, { line, col: 0 }, { line, col: END }, merged));
  }

  /** The row showing result line `line`, if loaded. */
  function findResultRow(line: number): ResultRowView | undefined {
    if (!info) return undefined;
    void results.version;
    // A line's row is the line plus the conflict headers above it.
    const guess = line + info.conflicts.filter((c) => c.resultRow <= line + info!.conflicts.indexOf(c)).length;
    for (const r of [guess, guess - 1, guess + 1, guess - 2, guess + 2]) {
      const row = results.row(r);
      if (row?.l?.n === line) return row;
    }
    return undefined;
  }

  async function waitRow(line: number): Promise<ResultRowView | undefined> {
    for (let i = 0; i < 20; i++) {
      await new Promise((r) => setTimeout(r, 25));
      const row = findResultRow(line);
      if (row) return row;
    }
    return undefined;
  }

  // ---- saving, undo ----

  async function save() {
    if (!info) return;
    await commit();
    await queue;
    if (!info) return;
    let markers = false;
    if (!info.clean) {
      const left = info.stats.unresolved;
      markers = await ask(
        `${left ? `${left} of ${info.stats.conflicts} conflicts are` : "Conflict markers are"} still in the result. Save with Git's conflict markers? Git then keeps ${info.title} marked as conflicted.`,
        { title: "Conflicts left", kind: "warning", okLabel: "Save with markers", cancelLabel: "Keep resolving" },
      );
      if (!markers) {
        const open = info.conflicts.find((c) => c.choice === "unresolved");
        if (open) goTo(open.block);
        return;
      }
    }
    try {
      const saved = await api.mergeSave(info.id, markers);
      apply(saved.info);
      onSaved?.(saved.clean);
      savedNote = saved.clean
        ? info.startup
          ? "Saved without conflicts. Close this window to return to git mergetool."
          : `Saved ${info.title} without conflicts.`
        : `Saved ${info.title} with conflict markers.`;
      notify(savedNote, "info");
    } catch (e) {
      error = String(e);
    }
  }

  function undo(redo: boolean) {
    if (!info) return;
    const id = info.id;
    editing = null;
    change(() => api.mergeUndo(id, redo));
  }

  function useGitResult() {
    if (!info) return;
    const id = info.id;
    change(() => api.mergeUseGitResult(id));
  }

  function onKey(e: KeyboardEvent) {
    if (e.target instanceof HTMLInputElement) return;
    const mod = e.ctrlKey || e.metaKey;
    if (e.key === "F8" || e.key === "F7") {
      e.preventDefault();
      step(e.key === "F8" && !e.shiftKey);
    } else if (mod && e.key === "s") {
      e.preventDefault();
      save();
    } else if (mod && (e.key === "z" || e.key === "Z")) {
      e.preventDefault();
      undo(e.shiftKey);
    } else if (mod && e.key === "y") {
      e.preventDefault();
      undo(true);
    } else if (e.altKey && current !== null && conflictNo.has(current)) {
      const choice: Choice | null =
        e.key === "ArrowLeft" ? "local" : e.key === "ArrowRight" ? "remote" : e.key === "ArrowUp" ? "base" : e.key === "ArrowDown" ? (e.shiftKey ? "remoteLocal" : "localRemote") : (e.key === "s" || e.key === "S") && suggestionText.has(current) ? "syntax" : null;
      if (choice) {
        e.preventDefault();
        choose(current, choice);
      }
    }
  }

  // ---- drawing ----

  interface Segment {
    text: string;
    color: string | null;
    mark: boolean;
  }

  function segments(line: LineView): Segment[] {
    const cuts = new Set<number>([0, line.t.length]);
    for (const [a, b] of line.s) cuts.add(a).add(b);
    for (const [a, b] of line.m) cuts.add(a).add(b);
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
      });
    }
    return out;
  }

  /** Tint of an input cell: which side changed it. */
  function inputTint(row: InputRowView, side: "l" | "b" | "r"): string {
    if (row.k === "unchanged") return "";
    if (row.k === "conflict") return "cf";
    if (side === "b") return "chg-b";
    if (row.k === "same") return side === "l" ? "chg-l" : "chg-r";
    return (row.k === "local" && side === "l") || (row.k === "remote" && side === "r") ? (side === "l" ? "chg-l" : "chg-r") : "";
  }

  function resultTint(row: ResultRowView): string {
    switch (row.c) {
      case "auto":
        return row.k === "local" ? "from-l" : row.k === "remote" ? "from-r" : row.k === "same" ? "from-both" : "";
      case "unresolved":
        return "cf";
      case "local":
        return "from-l";
      case "remote":
        return "from-r";
      case "localRemote":
      case "remoteLocal":
        return "from-both";
      case "base":
        return "from-b";
      case "edited":
        return "from-e";
      case "syntax":
        return "from-s";
    }
  }

  const choiceLabel: Record<Choice, string> = {
    auto: "merged automatically",
    unresolved: "not resolved",
    local: "took local",
    remote: "took remote",
    localRemote: "took both, local first",
    remoteLocal: "took both, remote first",
    base: "kept the base",
    edited: "edited by hand",
    syntax: "took the syntax-aware solution",
  };

  function rowsOf<T>(pane: RowPane<T>): [number, (T | undefined)[]] {
    void pane.version;
    const [first, last] = pane.window;
    const out: (T | undefined)[] = [];
    for (let i = first; i < last; i++) out.push(pane.row(i));
    return [first, out];
  }

  const topRows = $derived(rowsOf(inputs));
  const resultRows = $derived(rowsOf(results));

  function startSplit(e: PointerEvent) {
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const box = root!.getBoundingClientRect();
    const move = (m: PointerEvent) => {
      split = Math.min(0.85, Math.max(0.15, (m.clientY - box.top) / box.height));
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      inputs.viewHeight = topEl?.clientHeight ?? inputs.viewHeight;
      results.viewHeight = resultEl?.clientHeight ?? results.viewHeight;
      inputs.ensure();
      results.ensure();
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }
</script>

<!-- Keys that reach no element (after the button that had focus went away) still count while this tab shows. -->
<svelte:window onkeydown={(e) => e.target === document.body && root?.offsetParent && onKey(e)} />

<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div class="merge" bind:this={root} tabindex="0" onkeydown={onKey} role="application" aria-label="Merge">
  <div class="toolbar">
    <button onclick={() => step(false)} disabled={!info?.conflicts.length} title="Previous conflict (F7)">↑ Previous conflict</button>
    <button onclick={() => step(true)} disabled={!info?.conflicts.length} title="Next conflict (F8)">↓ Next conflict</button>
    {#if info}
      {#if !info.clean && !info.stats.unresolved}
        <span class="counter open" title="Saving asks whether to keep them">Conflict markers in the result</span>
      {:else if info.stats.conflicts === 0}
        <span class="counter done">No conflicts</span>
      {:else if info.stats.unresolved}
        <span class="counter open">{info.stats.unresolved} of {info.stats.conflicts} conflict{info.stats.conflicts > 1 ? "s" : ""} left</span>
      {:else}
        <span class="counter done">All {info.stats.conflicts} conflict{info.stats.conflicts > 1 ? "s" : ""} resolved</span>
      {/if}
    {/if}
    <span class="sep"></span>
    <button onclick={() => undo(false)} disabled={!info?.canUndo} title="Undo (Ctrl+Z)">↶ Undo</button>
    <button onclick={() => undo(true)} disabled={!info?.canRedo} title="Redo (Ctrl+Y)">↷ Redo</button>
    <span class="sep"></span>
    <div class="syntax">
      <button
        class="sa"
        class:on={syntaxMerge.enabled && !!syntaxMerge.found}
        onclick={() => (syntaxMenu = !syntaxMenu)}
        aria-expanded={syntaxMenu}
        title="Merge by the syntax of the file where lines conflict"
      >✦ Syntax-aware{structuredRunning ? " …" : ""} ▾</button>
      {#if syntaxMenu}
        <div class="menu" role="dialog" aria-label="Syntax-aware merging">
          <p class="note">Solutions for conflicts, from merging by the syntax of the file (struct fields, imports, keys) where lines conflict. They are offered, never applied by themselves.</p>
          <div class="choices" role="radiogroup" aria-label="Engine">
            <label>
              <input type="radio" name="syntax-engine" value="builtin" bind:group={syntaxMerge.engine} onchange={settingsChanged} />
              <span><b>Built in</b> <small>weave: merges functions, classes and keys as units</small></span>
            </label>
            <label>
              <input type="radio" name="syntax-engine" value="mergiraf" bind:group={syntaxMerge.engine} onchange={settingsChanged} />
              <span><b>mergiraf</b> <small>installed separately; also merges within a line</small></span>
            </label>
            <label>
              <input type="radio" name="syntax-engine" value="off" bind:group={syntaxMerge.engine} onchange={settingsChanged} />
              <span><b>Off</b> <small>lines only, as Git merges</small></span>
            </label>
          </div>
          {#if syntaxMerge.engine === "mergiraf"}
            <label class="path">
              <span>Program</span>
              <input bind:value={syntaxMerge.path} onchange={settingsChanged} placeholder="mergiraf on PATH" spellcheck="false" />
            </label>
          {/if}
          <p class="status">
            {#if !syntaxMerge.enabled}
              Off: conflicts are merged by lines only.
            {:else if structuredRunning}
              Merging with {syntaxMerge.found?.name ?? "the syntax-aware merge"}…
            {:else if syntaxMerge.problem}
              {syntaxMerge.problem}
            {:else if active && info?.structured?.declined}
              {info.structured.engine} does not merge this kind of file; the line merge stands.
            {:else if active && info?.structured}
              {info.structured.engine}: solved {info.structured.solved}{info.structured.narrowed ? `, narrowed ${info.structured.narrowed}` : ""} of {info.stats.conflicts} conflict{info.stats.conflicts === 1 ? "" : "s"}
            {:else if syntaxMerge.found}
              {syntaxMerge.found.name}
            {/if}
          </p>
          {#if syntaxMerge.engine === "mergiraf"}
            <p class="note">mergiraf (GPL-3.0) is run as a separate program, never bundled: <code>cargo install mergiraf</code> or your system's package.</p>
          {/if}
          <div class="row">
            <button onclick={() => runStructured(true)} disabled={!syntaxMerge.enabled || structuredRunning}>Run again</button>
            <button onclick={() => (syntaxMenu = false)}>Close</button>
          </div>
        </div>
      {/if}
    </div>
    {#if openSuggestions.length}
      <button class="sa-apply" onclick={applySuggestions} title="Take the syntax-aware solution for every conflict it solved">
        ✦ Apply {openSuggestions.length} solution{openSuggestions.length === 1 ? "" : "s"}
      </button>
    {/if}
    <span class="grow"></span>
    <button class="primary" onclick={save} disabled={!info} title="Write the result (Ctrl+S)">💾 Save result</button>
    {#if info?.startup}
      <button onclick={() => getCurrentWindow().close()} title="Close the window and return to git mergetool; it counts the merge as done once the result is saved without conflicts">Close</button>
    {/if}
  </div>

  {#if error}<div class="banner err">{error}</div>{/if}
  {#if info?.gitResult}
    <div class="banner warn">
      The file in the work tree is not what this merge gives (a merge driver or a recorded resolution may have resolved more).
      <button onclick={useGitResult}>Start from Git's result</button>
    </div>
  {/if}
  {#if structuredError && syntaxMerge.enabled}<div class="banner warn">Syntax-aware merge: {structuredError}</div>{/if}
  {#if info?.structured?.whole && active}
    <div class="banner warn">
      {#if info.stats.conflicts === 0 && info.structured.conflicts > 0}
        {info.structured.engine} found {info.structured.conflicts} conflict{info.structured.conflicts === 1 ? "" : "s"} the line merge did not, such as the same key or
        declaration added on both sides.
        <button onclick={useStructured}>Show its result</button>
      {:else}
        {info.structured.engine} merged this file differently from the line merge, also outside the conflicts{info.structured.conflicts
          ? `, and left ${info.structured.conflicts} conflict${info.structured.conflicts === 1 ? "" : "s"}`
          : ""}.
        <button onclick={useStructured}>Start from its result</button>
      {/if}
    </div>
  {/if}
  {#if loading}<div class="banner">Merging…</div>{/if}

  {#if info}
    <div class="heads">
      <div class="ph local"><span class="badge">LOCAL</span><span class="name" title={info.labels[0]}>{info.labels[0]}</span></div>
      <div class="ph base"><span class="badge">BASE</span><span class="name" title={info.labels[1]}>{info.labels[1]}</span></div>
      <div class="ph remote"><span class="badge">REMOTE</span><span class="name" title={info.labels[2]}>{info.labels[2]}</span></div>
    </div>
    <div
      class="pane top"
      style:flex-basis="{split * 100}%"
      bind:this={topEl}
      bind:clientHeight={inputs.viewHeight}
      onscroll={() => onScroll(0)}
      onwheel={(e) => onWheel(e, topEl, inputs)}
    >
      <div class="spacer" style:height="{spacerHeight(inputs.total * ROW)}px">
        <div class="window" style:transform="translateY({windowOffset(inputs.scrollTop, inputs.scale, topRows[0], ROW)}px)">
          {#each topRows[1] as row, k (topRows[0] + k)}
            {#if row}
              <div class="irow" class:current={row.i === current}>
                {#each [["l", row.l], ["b", row.b], ["r", row.r]] as const as [side, line] (side)}
                  <div class="cell {line ? inputTint(row, side) : 'fill'}">
                    {#if line}
                      <span class="no">{line.n + 1}</span><span class="code">{#if line.o}<span class="cut">…</span>{/if}{#each segments(line) as seg, si (si)}<span class:mark={seg.mark} style:color={seg.color}>{seg.text}</span>{/each}{#if line.x}<span class="cut">…</span>{/if}</span>
                    {/if}
                  </div>
                {/each}
              </div>
            {:else}
              <div class="irow loading"></div>
            {/if}
          {/each}
        </div>
      </div>
    </div>

    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="splitter" onpointerdown={startSplit} title="Drag to resize"></div>

    <div class="heads one">
      <div class="ph result">
        <span class="badge">RESULT</span><span class="name" title={info.output}>{info.output}</span>
        <span class="legend"><i class="sw l"></i>from local <i class="sw r"></i>from remote <i class="sw both"></i>same on both <i class="sw e"></i>edited{#if active && !info.structured?.declined} <i class="sw s"></i>syntax-aware{/if}</span>
        {#if info.dirty}<span class="dirty">● unsaved</span>{/if}
      </div>
    </div>
    <div
      class="pane bottom"
      bind:this={resultEl}
      bind:clientHeight={results.viewHeight}
      onscroll={() => onScroll(1)}
      onwheel={(e) => onWheel(e, resultEl, results)}
    >
      <div class="spacer" style:height="{spacerHeight(results.total * ROW)}px">
        <div class="window" style:transform="translateY({windowOffset(results.scrollTop, results.scale, resultRows[0], ROW)}px)">
          {#each resultRows[1] as row, k (resultRows[0] + k)}
            {#if !row}
              <div class="rrow loading"></div>
            {:else if !row.l}
              <div class="rrow header" class:open={row.c === "unresolved" || (row.c === "syntax" && conflictOf.get(row.i)?.partial)} class:current={row.i === current}>
                <span class="title">Conflict {conflictNo.get(row.i)} of {info.stats.conflicts}</span>
                {#if row.c === "unresolved"}
                  <button class="l" onclick={() => choose(row.i, "local")} title="Alt+←">← Take local</button>
                  <button class="r" onclick={() => choose(row.i, "remote")} title="Alt+→">Take remote →</button>
                  <button onclick={() => choose(row.i, "localRemote")} title="Alt+↓">Take both</button>
                  <button onclick={() => choose(row.i, "remoteLocal")} title="Alt+Shift+↓">Both, remote first</button>
                  <button onclick={() => choose(row.i, "base")} title="Alt+↑">Keep base</button>
                  {#if suggestionText.has(row.i)}
                    {#if conflictOf.get(row.i)?.partial}
                      <button class="s" onclick={() => choose(row.i, "syntax")} title={`Alt+S · ${info.structured?.engine ?? ""} solves part of this conflict and leaves a smaller one:\n${suggestionText.get(row.i)}`}>✦ Narrow down</button>
                    {:else}
                      <button class="s" onclick={() => choose(row.i, "syntax")} title={`Alt+S · ${info.structured?.engine ?? ""}'s solution:\n${suggestionText.get(row.i)}`}>✦ Syntax-aware</button>
                    {/if}
                  {/if}
                {:else}
                  <span class="state">{row.c === "syntax" && conflictOf.get(row.i)?.partial ? "narrowed by syntax; edit the rest by hand" : choiceLabel[row.c]}</span>
                  <button class="link" onclick={() => choose(row.i, "unresolved")}>Undo choice</button>
                {/if}
              </div>
            {:else}
              <!-- svelte-ignore a11y_click_events_have_key_events -->
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div class="rrow {resultTint(row)}" class:current={row.i === current && row.k === "conflict"} onclick={() => openEditor(row)}>
                <span class="no">{row.l.n + 1}</span>
                {#if editing?.line === row.l.n}
                  <input
                    class="editor"
                    bind:this={input}
                    bind:value={draft}
                    onkeydown={editKey}
                    onpaste={onPaste}
                    onblur={() => commit()}
                    spellcheck="false"
                    aria-label="Edit line {row.l.n + 1}"
                  />
                {:else}
                  <span class="code">{#if row.l.o}<span class="cut">…</span>{/if}{#each segments(row.l) as seg, si (si)}<span style:color={seg.color}>{seg.text}</span>{/each}{#if row.l.x}<span class="cut">…</span>{/if}</span>
                {/if}
              </div>
            {/if}
          {/each}
        </div>
      </div>
    </div>

    <div class="status">
      <span><b>{info.stats.automatic}</b> change{info.stats.automatic === 1 ? "" : "s"} merged automatically</span>
      <span>{info.stats.conflicts} conflict{info.stats.conflicts === 1 ? "" : "s"}</span>
      <span>{info.encoding}{info.syntax ? ` · ${info.syntax}` : ""}</span>
      <span class="grow"></span>
      {#if savedNote}<span>{savedNote}</span>{/if}
      <span>F7/F8 conflicts · Alt+←/→ take a side · Alt+↓ both{suggestionText.size ? " · Alt+S syntax-aware" : ""} · click a result line to edit</span>
    </div>
  {/if}
</div>

<style>
  .merge {
    --m-local: var(--accent);
    --m-remote: #a27ce8;
    --m-base: var(--similar);
    --m-conflict: var(--diff);
    --m-edited: var(--right-only);
    --t-local: color-mix(in srgb, var(--m-local) 16%, var(--bg));
    --t-remote: color-mix(in srgb, var(--m-remote) 16%, var(--bg));
    --t-base: color-mix(in srgb, var(--m-base) 14%, var(--bg));
    --t-conflict: color-mix(in srgb, var(--m-conflict) 17%, var(--bg));
    --t-both: color-mix(in srgb, var(--m-local) 9%, color-mix(in srgb, var(--m-remote) 12%, var(--bg)));
    --t-edited: color-mix(in srgb, var(--m-edited) 14%, var(--bg));
    --m-syntax: #1f9e7a;
    --t-syntax: color-mix(in srgb, var(--m-syntax) 15%, var(--bg));
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    outline: none;
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
  .sep {
    width: 1px;
    height: 18px;
    background: var(--line);
    margin: 0 4px;
  }
  .counter {
    font-size: 12px;
    padding: 2px 10px;
    border-radius: 10px;
    font-weight: 600;
  }
  .counter.open {
    background: var(--t-conflict);
    color: var(--m-conflict);
  }
  .counter.done {
    background: var(--t-edited);
    color: var(--m-edited);
  }
  button.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .banner {
    padding: 6px 12px;
    background: var(--panel);
    border-bottom: 1px solid var(--line);
  }
  .banner.err {
    color: var(--error);
  }
  .banner.warn {
    color: var(--similar);
  }
  .heads {
    display: grid;
    grid-template-columns: 1fr 1fr 1fr;
    gap: 1px;
    background: var(--line);
    border-bottom: 1px solid var(--line);
  }
  .heads.one {
    grid-template-columns: 1fr;
  }
  .ph {
    background: var(--panel-2, var(--panel));
    padding: 4px 10px;
    display: flex;
    gap: 8px;
    align-items: center;
    min-width: 0;
    font-size: 12px;
    border-top: 2px solid var(--c);
  }
  .ph.local {
    --c: var(--m-local);
  }
  .ph.base {
    --c: var(--m-base);
  }
  .ph.remote {
    --c: var(--m-remote);
  }
  .ph.result {
    --c: var(--m-edited);
  }
  .badge {
    font-size: 9.5px;
    font-weight: 700;
    letter-spacing: 0.7px;
    color: var(--c);
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }
  .legend {
    margin-left: auto;
    color: var(--dim);
    font-size: 11px;
    display: flex;
    align-items: center;
    gap: 5px;
    white-space: nowrap;
  }
  .sw {
    display: inline-block;
    width: 9px;
    height: 9px;
    border-radius: 2px;
    margin-left: 6px;
  }
  .sw.l {
    background: var(--m-local);
  }
  .sw.r {
    background: var(--m-remote);
  }
  .sw.both {
    background: linear-gradient(90deg, var(--m-local) 50%, var(--m-remote) 50%);
  }
  .sw.e {
    background: var(--m-edited);
  }
  .dirty {
    color: var(--similar);
    white-space: nowrap;
  }
  .pane {
    overflow: auto;
    position: relative;
    min-height: 60px;
    font-family: var(--mono);
    font-size: 12.5px;
    background: var(--bg);
  }
  .pane.top {
    flex: 0 0 50%;
  }
  .pane.bottom {
    flex: 1 1 0;
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
  .splitter {
    height: 6px;
    flex: none;
    cursor: row-resize;
    background: var(--chrome);
    border-top: 1px solid var(--line);
    border-bottom: 1px solid var(--line);
  }
  .irow {
    display: grid;
    grid-template-columns: 1fr 1fr 1fr;
    height: 19px;
  }
  .irow.loading,
  .rrow.loading {
    height: 19px;
  }
  .cell,
  .rrow {
    display: flex;
    min-width: 0;
    height: 19px;
    line-height: 19px;
    white-space: pre;
    overflow: hidden;
  }
  .cell + .cell {
    border-left: 1px solid var(--line);
  }
  .cell.fill {
    background: var(--filler);
  }
  .no {
    flex: none;
    width: 46px;
    padding-right: 8px;
    text-align: right;
    color: var(--faint);
    font-size: 11px;
    user-select: none;
  }
  .code {
    overflow: hidden;
    text-overflow: clip;
  }
  .cut {
    color: var(--faint);
  }
  .mark {
    background: var(--mark);
    border-radius: 2px;
  }
  .chg-l {
    background: var(--t-local);
  }
  .chg-r {
    background: var(--t-remote);
  }
  .chg-b {
    background: var(--t-base);
  }
  .cf {
    background: var(--t-conflict);
  }
  .irow.current .cell {
    box-shadow: inset 0 1px 0 var(--m-conflict), inset 0 -1px 0 var(--m-conflict);
  }
  .rrow {
    cursor: text;
  }
  .from-l {
    background: var(--t-local);
    box-shadow: inset 3px 0 0 var(--m-local);
  }
  .from-r {
    background: var(--t-remote);
    box-shadow: inset 3px 0 0 var(--m-remote);
  }
  .from-both {
    background: var(--t-both);
    box-shadow: inset 3px 0 0 var(--m-local), inset 6px 0 0 var(--m-remote);
  }
  .from-b {
    background: var(--t-base);
    box-shadow: inset 3px 0 0 var(--m-base);
  }
  .from-e {
    background: var(--t-edited);
    box-shadow: inset 3px 0 0 var(--m-edited);
  }
  .from-s {
    background: var(--t-syntax);
    box-shadow: inset 3px 0 0 var(--m-syntax);
  }
  .sw.s {
    background: var(--m-syntax);
  }
  .rrow.header button.s {
    color: var(--m-syntax);
    border-color: var(--m-syntax);
    font-weight: 600;
  }
  .syntax {
    position: relative;
  }
  .sa.on {
    color: var(--m-syntax);
    border-color: var(--m-syntax);
  }
  .sa-apply {
    color: #fff;
    background: var(--m-syntax);
    border-color: var(--m-syntax);
  }
  .syntax .menu {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 30;
    width: 340px;
    padding: 12px;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 6px;
    box-shadow: 0 6px 24px rgb(0 0 0 / 0.25);
    display: flex;
    flex-direction: column;
    gap: 8px;
    font-size: 12px;
  }
  .menu label {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .menu .path span {
    color: var(--dim);
    flex: none;
  }
  .menu .path input {
    flex: 1;
    min-width: 0;
    font-family: var(--mono);
  }
  .menu p {
    margin: 0;
  }
  .menu .status {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .menu .note {
    color: var(--dim);
  }
  .menu .choices {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .menu .choices label {
    align-items: flex-start;
  }
  .menu .choices small {
    display: block;
    color: var(--dim);
  }
  .menu .row {
    display: flex;
    gap: 6px;
    justify-content: flex-end;
  }
  .rrow.cf .code {
    color: var(--dim);
  }
  .rrow.header {
    font-family: var(--ui);
    font-size: 11.5px;
    gap: 6px;
    align-items: center;
    padding: 0 8px 0 54px;
    background: var(--panel);
    border-top: 1px solid var(--line);
    cursor: default;
    overflow: hidden;
  }
  .rrow.header.open {
    background: var(--t-conflict);
    border-top-color: var(--m-conflict);
  }
  .rrow.header.current {
    box-shadow: inset 3px 0 0 var(--m-conflict);
  }
  .rrow.header .title {
    font-weight: 700;
    color: var(--m-conflict);
    margin-right: 4px;
  }
  .rrow.header:not(.open) .title {
    color: var(--dim);
  }
  .rrow.header button {
    font-size: 11px;
    padding: 0 7px;
    line-height: 16px;
  }
  .rrow.header button.l {
    color: var(--m-local);
  }
  .rrow.header button.r {
    color: var(--m-remote);
  }
  .rrow.header button.link {
    border: 0;
    background: none;
    color: var(--accent);
    text-decoration: underline;
  }
  .state {
    color: var(--dim);
  }
  .editor {
    flex: 1;
    font: inherit;
    height: 19px;
    border: 0;
    outline: 1px solid var(--accent);
    background: var(--input);
    color: var(--text);
    padding: 0;
  }
  .status {
    display: flex;
    flex-wrap: wrap;
    gap: 14px;
    padding: 4px 10px;
    font-size: 11.5px;
    color: var(--dim);
    border-top: 1px solid var(--line);
    background: var(--panel);
  }
  .status b {
    color: var(--text);
  }
</style>
