<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import { api, defaultDiffOptions, theme, type FileDiffInfo, type LineView, type RowView, type SessionInfo } from "./api";
  import PathBar from "./PathBar.svelte";
  import { formatSize } from "./format";

  let { session, path }: { session: SessionInfo; path: string } = $props();

  const ROW = 19;
  const CHUNK = 200;
  const CHAR = 7.6;

  let options = $state(defaultDiffOptions());
  let syntaxOverride = $state<string>("");
  let syntaxNames = $state<string[]>([]);
  let info = $state<FileDiffInfo | null>(null);
  let error = $state("");
  let loading = $state(true);

  // Rows are fetched lazily in chunks; the counter re-renders when chunks arrive.
  let cache = new Map<number, RowView>();
  let pending = new Set<number>();
  let cacheVersion = $state(0);

  let viewport = $state<HTMLDivElement>();
  let scrollTop = $state(0);
  let viewHeight = $state(600);
  let hscroll = $state(0);
  let selected = $state<number | null>(null);
  let section = $state(-1);

  const total = $derived(info?.rows ?? 0);
  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - 20));
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

  async function load() {
    loading = true;
    error = "";
    const previous = info?.id;
    try {
      info = await api.openFileDiff(session.id, path, $state.snapshot(options), syntaxOverride || null);
      cache = new Map();
      pending = new Set();
      cacheVersion++;
      section = -1;
      if (previous !== undefined) api.closeFileDiff(previous);
      await tick();
      if (info.sections.length) goTo(0);
      else ensureLoaded();
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function fetchChunk(chunk: number) {
    if (!info || pending.has(chunk)) return;
    pending.add(chunk);
    const id = info.id;
    try {
      const rows = await api.fileRows(id, chunk * CHUNK, CHUNK, theme());
      if (info?.id !== id) return;
      rows.forEach((row, i) => cache.set(chunk * CHUNK + i, row));
      cacheVersion++;
    } catch (e) {
      pending.delete(chunk);
      error = String(e);
    }
  }

  function ensureLoaded() {
    if (!info) return;
    for (let c = Math.floor(first / CHUNK); c <= Math.floor(Math.max(first, last - 1) / CHUNK); c++) fetchChunk(c);
  }

  $effect(() => {
    void first;
    void last;
    ensureLoaded();
  });

  function goTo(i: number) {
    if (!info || !viewport || !info.sections.length) return;
    section = Math.max(0, Math.min(info.sections.length - 1, i));
    const start = info.sections[section].start;
    selected = start;
    viewport.scrollTop = Math.max(0, (start - 3) * ROW);
  }

  function next(dir: 1 | -1) {
    if (!info?.sections.length) return;
    if (section === -1) {
      const top = Math.floor(scrollTop / ROW);
      const idx = info.sections.findIndex((s) => s.start > top);
      goTo(dir === 1 ? (idx === -1 ? info.sections.length - 1 : idx) : Math.max(0, (idx === -1 ? info.sections.length : idx) - 1));
    } else {
      goTo(section + dir);
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "F8" || (e.altKey && e.key === "ArrowDown") || e.key === "n") next(1);
    else if (e.key === "F7" || (e.altKey && e.key === "ArrowUp") || e.key === "p") next(-1);
    else return;
    e.preventDefault();
  }

  function onWheel(e: WheelEvent) {
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
    viewport.scrollTop = Math.max(0, row * ROW - viewHeight / 2);
    section = -1;
  }

  interface Segment {
    text: string;
    color: string | null;
    mark: boolean;
  }

  /** Splits a line at every syntax-colour and change-mark boundary. */
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

  const eolLabel = (s: FileDiffInfo["left"]) =>
    !s.exists ? "missing" : s.binary ? `binary · ${formatSize(s.size)}` : `${s.encoding}${s.bom ? " BOM" : ""} · ${s.lineEnding}`;

  onMount(() => {
    load();
    api.syntaxNames().then((n) => (syntaxNames = n));
    const media = window.matchMedia("(prefers-color-scheme: light)");
    const onTheme = () => {
      cache = new Map();
      pending = new Set();
      cacheVersion++;
    };
    media.addEventListener("change", onTheme);
    return () => media.removeEventListener("change", onTheme);
  });

  onDestroy(() => {
    if (info) api.closeFileDiff(info.id);
  });
</script>

<div class="view">
  <div class="toolbar">
    <button onclick={() => next(-1)} disabled={!info?.sections.length} title="Previous difference (F7)">◀ Prev</button>
    <button onclick={() => next(1)} disabled={!info?.sections.length} title="Next difference (F8)">Next ▶</button>
    {#if info}
      <span class="count">{section >= 0 ? section + 1 : "–"} / {info.sections.length}</span>
    {/if}
    <span class="sep"></span>
    <select bind:value={options.whitespace} onchange={load} title="Whitespace rule">
      <option value="exact">Whitespace: exact</option>
      <option value="normalize">Whitespace: ignore changes</option>
      <option value="ignoreAll">Whitespace: ignore all</option>
    </select>
    <button class:on={options.ignoreCase} onclick={() => { options.ignoreCase = !options.ignoreCase; load(); }}>Ignore case</button>
    <button class:on={options.minimal} onclick={() => { options.minimal = !options.minimal; load(); }} title="Myers (minimal) instead of histogram">Minimal diff</button>
    <span class="sep"></span>
    <select bind:value={syntaxOverride} onchange={load} title="Syntax highlighting">
      <option value="">Syntax: auto{info?.syntax && !syntaxOverride ? ` (${info.syntax})` : ""}</option>
      {#each syntaxNames as name (name)}
        <option value={name}>{name}</option>
      {/each}
    </select>
  </div>

  <div class="pathbars">
    <PathBar info={session.left} suffix={path} />
    <PathBar info={session.right} suffix={path} />
  </div>
  {#if info}
    <div class="meta">
      <span>{eolLabel(info.left)}</span>
      <span>{eolLabel(info.right)}</span>
    </div>
  {/if}

  {#if error}
    <div class="banner err">{error}</div>
  {:else if info?.left.binary || info?.right.binary}
    <div class="banner">
      Binary files {info.identical ? "are identical" : "differ"} ({formatSize(info.left.size)} vs {formatSize(info.right.size)}).
    </div>
  {:else if info?.identical}
    <div class="banner ok">The files are identical.</div>
  {:else if info && !info.sections.length && !loading}
    <div class="banner ok">No differences under the current rules{info.stats.minor ? ` (${info.stats.minor} lines differ only in ignored whitespace or case)` : ""}.</div>
  {/if}

  <div class="body">
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="overview" onclick={overviewClick} title="Overview: click to jump">
      {#if info && total}
        {#each info.sections as s, i (i)}
          <i style:top="{(s.start / total) * 100}%" style:height="max(2px, {((s.end - s.start) / total) * 100}%)"></i>
        {/each}
        <b style:top="{((scrollTop / ROW) / total) * 100}%" style:height="{Math.min(100, (viewHeight / ROW / total) * 100)}%"></b>
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
      <div class="spacer" style:height="{total * ROW}px">
        <div class="window" style:transform="translateY({first * ROW}px)">
          {#each visible as { index, row } (index)}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div class="row {row?.k ?? 'equal'}" class:cur={index === selected} role="row" tabindex="-1" onclick={() => (selected = index)}>
              {#each [row?.l, row?.r] as line, side (side)}
                {#if side === 1}
                  <div class="mid">{row && row.k !== "equal" ? { minor: "≈", changed: "≠", removed: "−", added: "+" }[row.k] : ""}</div>
                {/if}
                {#if line}
                  <div class="ln">{line.n + 1}</div>
                  <div class="code"><span style:transform="translateX({-hscroll}px)">{#each segments(line) as seg, si (si)}<span class:mk={seg.mark} style:color={seg.color}>{seg.text}</span>{/each}</span></div>
                {:else if row}
                  <div class="ln fill"></div>
                  <div class="code fill"></div>
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

  <div class="detail mono">
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
    {:else if info}
      <span><b>{info.sections.length}</b> difference sections</span>
      <span class="c-diff">{info.stats.changed} changed</span>
      <span class="c-add">{info.stats.added} added</span>
      <span class="c-rem">{info.stats.removed} removed</span>
      {#if selected !== null}<span>Row {selected + 1} of {total}</span>{/if}
      <span>{info.syntax ?? "Plain text"}</span>
      <span class="hint">F7 / F8: previous / next difference · Shift+wheel: scroll sideways</span>
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
    grid-template-columns: 48px minmax(0, 1fr) 22px 48px minmax(0, 1fr);
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
  .code > span {
    display: inline-block;
  }
  .mid {
    background: var(--panel-2);
    border-left: 1px solid var(--line);
    border-right: 1px solid var(--line);
    text-align: center;
    color: var(--dim);
    font-size: 11px;
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
