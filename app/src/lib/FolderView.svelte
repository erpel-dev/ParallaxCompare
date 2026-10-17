<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { ask } from "@tauri-apps/plugin-dialog";
  import { api, defaultCompareOptions, type CompareResult, type SessionInfo, type Side } from "./api";
  import PathBar from "./PathBar.svelte";
  import { formatSize, formatTime } from "./format";
  import { allFolders, countFiles, countNodes, differingFolders, findNode, flatten, replaceNode, type Filter } from "./tree";

  let {
    session,
    onOpenFile,
    onSwap,
  }: { session: SessionInfo; onOpenFile: (path: string) => void; onSwap: () => void } = $props();

  const ROW = 22;

  let options = $state(defaultCompareOptions());
  let excludesText = $state(options.excludes.join(", "));
  let result = $state.raw<CompareResult | null>(null);
  // The result tree is large and mutated in place; this counter drives re-rendering.
  let version = $state(0);
  let expanded = new Set<string>();
  let filter = $state<Filter>("all");
  let selected = $state<string | null>(null);
  let running = $state(false);
  let progress = $state(0);
  let error = $state("");
  let busy = $state("");

  let viewport = $state<HTMLDivElement>();
  let scrollTop = $state(0);
  let viewHeight = $state(600);

  const rows = $derived.by(() => {
    void version;
    return result ? flatten(result.root, expanded, filter) : [];
  });
  const counts = $derived.by(() => {
    void version;
    return result ? countFiles(result.root) : null;
  });
  const first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - 10));
  const last = $derived(Math.min(rows.length, first + Math.ceil(viewHeight / ROW) + 20));
  const selectedIndex = $derived(selected === null ? -1 : rows.findIndex((r) => r.path === selected));
  const selectedRow = $derived(selectedIndex >= 0 ? rows[selectedIndex] : null);

  async function run() {
    running = true;
    error = "";
    progress = 0;
    try {
      const r = await api.runCompare(session.id, $state.snapshot(options));
      // Open folders with differences, but keep huge trees collapsed.
      expanded = new Set(differingFolders(r.root, countNodes(r.root) > 20000 ? 0 : 400));
      result = r;
      version++;
    } catch (e) {
      if (String(e) !== "cancelled") error = String(e);
    } finally {
      running = false;
    }
  }

  onMount(() => {
    run();
    const unlisten = listen<{ session: number; files: number }>("compare-progress", (e) => {
      if (e.payload.session === session.id) progress = e.payload.files;
    });
    return () => {
      unlisten.then((f) => f());
    };
  });

  function applyExcludes() {
    const next = excludesText
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
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
          const fresh = await api.expand(session.id, path, $state.snapshot(options));
          node.children = fresh.children;
          node.unexpanded = false;
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

  function setAll(open: boolean) {
    if (!result) return;
    expanded = open ? new Set(allFolders(result.root)) : new Set();
    version++;
  }

  function activate(path: string, container: boolean) {
    selected = path;
    if (container) toggle(path);
    else onOpenFile(path);
  }

  async function ensureVisible(index: number) {
    await tick();
    if (!viewport) return;
    const top = index * ROW;
    if (top < viewport.scrollTop + ROW) viewport.scrollTop = Math.max(0, top - ROW);
    else if (top + ROW > viewport.scrollTop + viewHeight) viewport.scrollTop = top + 2 * ROW - viewHeight;
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
        if (!running) run();
        e.preventDefault();
        return;
      case "Enter":
        if (row) activate(row.path, row.container);
        e.preventDefault();
        return;
      default:
        return;
    }
    e.preventDefault();
    if (index >= 0) {
      selected = rows[index].path;
      ensureVisible(index);
    }
  }

  async function copy(toRight: boolean) {
    const row = selectedRow;
    if (!row || !result) return;
    busy = `Copying ${row.path}…`;
    try {
      const node = await api.copyEntry(session.id, row.path, toRight, $state.snapshot(options));
      replaceNode(result.root, row.path, node);
      version++;
    } catch (e) {
      error = String(e);
    } finally {
      busy = "";
    }
  }

  async function remove(left: boolean) {
    const row = selectedRow;
    if (!row || !result) return;
    const side = left ? session.left.display : session.right.display;
    const ok = await ask(`Delete "${row.path}" from ${side}?\n\nThis cannot be undone.`, {
      title: "Delete",
      kind: "warning",
    });
    if (!ok) return;
    try {
      const node = await api.deleteEntry(session.id, row.path, left, $state.snapshot(options));
      replaceNode(result.root, row.path, node);
      version++;
    } catch (e) {
      error = String(e);
    }
  }

  const canCopyRight = $derived(!!selectedRow?.node.left && session.right.writable);
  const canCopyLeft = $derived(!!selectedRow?.node.right && session.left.writable);

  const glyph = { same: "=", similar: "≈", different: "≠", leftOnly: "◀", rightOnly: "▶", error: "!" } as const;

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
    <button onclick={run} disabled={running} title="Compare again (F5)">⟳ Refresh</button>
    <button onclick={onSwap} title="Swap left and right">⇄ Swap</button>
    <span class="sep"></span>
    {#each [["all", "All"], ["diffs", "Diffs"], ["same", "Same"], ["orphans", "Orphans"]] as const as [id, label] (id)}
      <button class:on={filter === id} onclick={() => { filter = id; version++; }}>{label}</button>
    {/each}
    <span class="sep"></span>
    <button onclick={() => setAll(true)} title="Expand all folders">Expand</button>
    <button onclick={() => setAll(false)} title="Collapse all folders">Collapse</button>
    <span class="sep"></span>
    <button disabled={!canCopyRight} onclick={() => copy(true)} title="Copy the selection to the right side">Copy →</button>
    <button disabled={!canCopyLeft} onclick={() => copy(false)} title="Copy the selection to the left side">← Copy</button>
    <button disabled={!selectedRow?.node.left || !session.left.writable} onclick={() => remove(true)} title="Delete the selection on the left side">Delete ◀</button>
    <button disabled={!selectedRow?.node.right || !session.right.writable} onclick={() => remove(false)} title="Delete the selection on the right side">Delete ▶</button>
    <span class="sep"></span>
    <select bind:value={options.mode} onchange={run} title="Compare rule">
      <option value="content">Content</option>
      <option value="quick">Size &amp; date</option>
    </select>
    <label class="check"><input type="checkbox" bind:checked={options.ignoreLineEndings} onchange={run} /> Ignore CRLF/LF</label>
    <label class="check" title="Documents (PDF, Word, Excel, PowerPoint, OpenDocument) with the same text and images with the same pixels are marked ≈ instead of ≠">
      <input type="checkbox" bind:checked={options.compareContents} onchange={run} /> By content
    </label>
    <input
      type="text"
      class="excludes mono"
      bind:value={excludesText}
      onkeydown={(e) => e.key === "Enter" && applyExcludes()}
      onblur={applyExcludes}
      title="Excluded names or paths (globs, comma separated)"
      placeholder="Exclude: *.o, build/**"
    />
  </div>

  <div class="pathbars">
    <PathBar info={session.left} />
    <PathBar info={session.right} />
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
    onkeydown={onKey}
    tabindex="0"
    role="tree"
  >
    {#if result && rows.length === 0 && !running}
      <div class="empty">{result.root.status === "same" ? "Both sides are identical." : "Nothing matches the current filter."}</div>
    {/if}
    <div class="spacer" style:height="{rows.length * ROW}px">
      <div class="window" style:transform="translateY({first * ROW}px)">
        {#each rows.slice(first, last) as row (row.path)}
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <div
            class="row {row.node.status}"
            class:sel={row.path === selected}
            role="treeitem"
            aria-selected={row.path === selected}
            aria-expanded={row.container ? row.expanded : undefined}
            tabindex="-1"
            onclick={() => (selected = row.path)}
            ondblclick={() => activate(row.path, row.container)}
            title={row.node.message ?? ""}
          >
            {#each [row.node.left, row.node.right] as side, i (i)}
              {#if i === 1}
                <div class="mid">{glyph[row.node.status]}</div>
              {/if}
              {#if side}
                <div class="side">
                  <span class="name" style:padding-left="{row.depth * 16}px">
                    {#if row.container}
                      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
                      <span class="chev" onclick={(e) => { e.stopPropagation(); toggle(row.path); }}>{row.expanded ? "▾" : "▸"}</span>
                    {:else}
                      <span class="chev"></span>
                    {/if}
                    <span class="icon">{icon(side)}</span>{row.node.name}
                  </span>
                  <span class="num">{side.kind === "dir" ? "" : formatSize(side.size)}</span>
                  <span class="num">{formatTime(side.mtime)}</span>
                </div>
              {:else}
                <div class="side filler"></div>
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
      <span><b>{counts.files.toLocaleString()}</b> files</span>
      <span class="c-same">{counts.same.toLocaleString()} same</span>
      {#if counts.similar}<span class="c-similar" title="Equal content, different bytes">{counts.similar.toLocaleString()} similar</span>{/if}
      <span class="c-diff">{counts.different.toLocaleString()} different</span>
      <span class="c-left">{counts.leftOnly.toLocaleString()} left only</span>
      <span class="c-right">{counts.rightOnly.toLocaleString()} right only</span>
      {#if counts.errors}<span class="c-err">{counts.errors} errors</span>{/if}
      <span>compared in <b>{s.elapsedMs < 1000 ? `${s.elapsedMs} ms` : `${(s.elapsedMs / 1000).toFixed(2)} s`}</b>{s.identicalSubtrees ? ` · ${s.identicalSubtrees.toLocaleString()} identical folders skipped via Git tree ids` : ""}</span>
    {/if}
    {#if busy}<span>{busy}</span>{/if}
    {#if error}<span class="c-err" title={error}>{error}</span>{/if}
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
  .check {
    display: flex;
    gap: 4px;
    align-items: center;
    margin: 0 4px;
  }
  .excludes {
    width: 130px;
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
  .viewport:focus .row.sel {
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
  .c-diff {
    color: var(--diff);
  }
  .c-similar {
    color: var(--similar);
  }
  .c-left {
    color: var(--left-only);
  }
  .c-right {
    color: var(--right-only);
  }
  .c-err {
    color: var(--error);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 60ch;
  }
</style>
