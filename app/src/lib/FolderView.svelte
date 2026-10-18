<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { ask } from "@tauri-apps/plugin-dialog";
  import { api, defaultCompareOptions, type CompareResult, type Node, type OpResult, type PathsChanged, type SessionInfo, type Side } from "./api";
  import Nav, { type NavActions } from "./Nav.svelte";
  import PathBar, { type SideEdit } from "./PathBar.svelte";
  import { formatSize, formatTime } from "./format";
  import {
    allFolders,
    countFiles,
    countNodes,
    differingFolders,
    filesBelow,
    findNode,
    flatten,
    pairRows,
    refreshTarget,
    replaceNode,
    type Filter,
    type RowSide,
  } from "./tree";

  let {
    session,
    onOpenFile,
    onSwap,
    onCompareRows,
    edit,
    nav,
    reveal,
  }: {
    session: SessionInfo;
    onOpenFile: (path: string) => void;
    onSwap: () => void;
    onCompareRows?: (a: RowSide, b: RowSide) => void;
    edit?: [SideEdit, SideEdit];
    nav?: NavActions;
    /** A row to select and scroll to, e.g. the file just left with "up"; `n` repeats a request. */
    reveal?: { path: string; n: number };
  } = $props();

  const ROW = 22;

  let options = $state(defaultCompareOptions());
  let excludesText = $state(options.excludes.join(", "));
  let result = $state.raw<CompareResult | null>(null);
  // The result tree is large and mutated in place; this counter drives re-rendering.
  let version = $state(0);
  let expanded = new Set<string>();
  let filter = $state<Filter>("all");
  /** The focused row; `selection` holds all selected rows (Ctrl/Shift+click). */
  let selected = $state<string | null>(null);
  let selection = $state(new Set<string>());
  let anchor: string | null = null;
  let running = $state(false);
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

  async function run(invalidate = false) {
    running = true;
    error = "";
    progress = 0;
    try {
      const r = await api.runCompare(session.id, $state.snapshot(options), invalidate);
      // Open folders with differences, but keep huge trees collapsed.
      expanded = new Set(differingFolders(r.root, countNodes(r.root) > 20000 ? 0 : 400));
      result = r;
      version++;
    } catch (e) {
      if (String(e) !== "cancelled") error = String(e);
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
      listen<{ session: number; files: number }>("compare-progress", (e) => {
        if (e.payload.session === session.id) progress = e.payload.files;
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
    selectOnly(path);
    if (container) toggle(path);
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
        if (!running) run(true);
        e.preventDefault();
        return;
      case "Enter":
        if (row) activate(row.path, row.container);
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
      const node = findNode(result!.root, p);
      return !!(left ? node?.left : node?.right);
    });
  }

  function applyResult(paths: string[], r: OpResult) {
    if (!result) return;
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
    const paths = present(!toRight);
    if (!paths.length) return;
    // Files on the target side that the copy replaces, and those newer than their source.
    const replaced: string[] = [];
    const newer: string[] = [];
    for (const p of paths) {
      for (const f of filesBelow(findNode(result.root, p)!, p)) {
        const [src, dst] = toRight ? [f.node.left, f.node.right] : [f.node.right, f.node.left];
        if (!src || !dst || f.node.status === "same") continue;
        replaced.push(f.path);
        if (src.mtime !== null && dst.mtime !== null && dst.mtime > src.mtime) newer.push(f.path);
      }
    }
    const side = toRight ? "right" : "left";
    if (replaced.length) {
      const message =
        `Copying replaces ${replaced.length} file${replaced.length > 1 ? "s" : ""} on the ${side} side:\n${list(replaced)}` +
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
    const paths = present(left);
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
    try {
      const nodes = await api.refreshPaths(session.id, outer, $state.snapshot(options));
      if (result?.root !== root) return;
      outer.forEach((p, i) => replaceNode(root, p, nodes[i]));
      version++;
    } catch (e) {
      error = String(e);
    }
    flushChanges();
  }

  const canCopy = $derived.by(() => {
    void version;
    return [session.left.writable && present(false).length > 0, session.right.writable && present(true).length > 0];
  });
  const canDelete = $derived.by(() => {
    void version;
    return [session.left.writable && present(true).length > 0, session.right.writable && present(false).length > 0];
  });

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
    {#if nav}<Nav {nav} /><span class="sep"></span>{/if}
    <button onclick={() => run(true)} disabled={running} title="Compare again (F5)">⟳ Refresh</button>
    <button onclick={onSwap} title="Swap left and right">⇄ Swap</button>
    <button
      disabled={!pair || !onCompareRows}
      onclick={() => pair && onCompareRows?.(pair[0], pair[1])}
      title="Compare the two selected rows with each other, e.g. a file renamed between the sides (select with Ctrl+click)"
    >⇆ Compare these</button>
    <span class="sep"></span>
    {#each [["all", "All"], ["diffs", "Diffs"], ["same", "Same"], ["orphans", "Orphans"]] as const as [id, label] (id)}
      <button class:on={filter === id} onclick={() => { filter = id; version++; }}>{label}</button>
    {/each}
    <span class="sep"></span>
    <button onclick={() => setAll(true)} title="Expand all folders">Expand</button>
    <button onclick={() => setAll(false)} title="Collapse all folders">Collapse</button>
    <span class="sep"></span>
    <button disabled={!canCopy[1] || !!op} onclick={() => copy(true)} title="Copy the selection to the right side">Copy →</button>
    <button disabled={!canCopy[0] || !!op} onclick={() => copy(false)} title="Copy the selection to the left side">← Copy</button>
    <button disabled={!canDelete[0] || !!op} onclick={() => remove(true)} title="Delete the selection on the left side">Delete ◀</button>
    <button disabled={!canDelete[1] || !!op} onclick={() => remove(false)} title="Delete the selection on the right side">Delete ▶</button>
    <span class="sep"></span>
    <select bind:value={options.mode} onchange={() => run()} title="Compare rule">
      <option value="content">Content</option>
      <option value="quick">Size &amp; date</option>
    </select>
    <label class="check"><input type="checkbox" bind:checked={options.ignoreLineEndings} onchange={() => run()} /> Ignore CRLF/LF</label>
    <label class="check" title="Documents (PDF, Word, Excel, PowerPoint, OpenDocument) with the same text and images with the same pixels are marked ≈ instead of ≠">
      <input type="checkbox" bind:checked={options.compareContents} onchange={() => run()} /> By content
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
    <PathBar info={session.left} edit={edit?.[0]} title="Left" />
    <PathBar info={session.right} edit={edit?.[1]} title="Right" />
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
            class:sel={selection.has(row.path)}
            class:focus={row.path === selected}
            role="treeitem"
            aria-selected={selection.has(row.path)}
            aria-expanded={row.container ? row.expanded : undefined}
            tabindex="-1"
            onclick={(e) => click(e, row.path)}
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
    {#if op}
      <span>{op.label}…{op.total ? ` ${op.done.toLocaleString()} / ${op.total.toLocaleString()}` : ""}</span>
      <button class="small" onclick={() => api.cancelOp(session.id)}>Cancel</button>
    {/if}
    {#if targets.length > 1}<span>{targets.length} selected</span>{/if}
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
