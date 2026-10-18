<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { ask } from "@tauri-apps/plugin-dialog";
  import FolderView from "./lib/FolderView.svelte";
  import NewCompare from "./lib/NewCompare.svelte";
  import type { SideEdit } from "./lib/PathBar.svelte";
  import TextView from "./lib/TextView.svelte";
  import { api, type SessionInfo, type SourceOptions, type SourceSpec } from "./lib/api";
  import { baseName } from "./lib/format";

  interface Request {
    left: SourceSpec;
    right: SourceSpec;
    options: SourceOptions;
    mergeBase: boolean;
  }

  type Tab =
    | { key: number; kind: "new"; title: string }
    | { key: number; kind: "folder"; title: string; session: SessionInfo; request: Request }
    | {
        key: number;
        kind: "text";
        title: string;
        session: SessionInfo;
        path: string;
        /** Set for a comparison of two files; a file opened from a folder comparison shares that session. */
        request?: Request;
        dirty?: boolean;
      };
  type SessionTab = Exclude<Tab, { kind: "new" }>;
  type SideName = "left" | "right";

  let tabs = $state<Tab[]>([]);
  let active = $state(0);
  let nextKey = 1;
  let error = $state("");
  /** The new-comparison forms, by tab, to fill in dropped paths. */
  let pickers: Record<number, NewCompare> = $state({});
  /** Files being dragged over the window, and the side they would replace. */
  let drop = $state<{ side: SideName | "both"; count: number } | null>(null);
  let mainEl = $state<HTMLElement>();

  function add(tab: Tab) {
    tabs.push(tab);
    active = tab.key;
  }

  function newTab() {
    add({ key: nextKey++, kind: "new", title: "New comparison" });
  }

  function titleFor(session: SessionInfo): string {
    const side = (s: SessionInfo["left"]) => (s.rev ? s.rev : baseName(s.display));
    return `${side(session.left)} ↔ ${side(session.right)}`;
  }

  function sessionTab(session: SessionInfo, request: Request): Tab {
    const key = nextKey++;
    return session.isFile
      ? { key, kind: "text", title: titleFor(session), session, path: "", request }
      : { key, kind: "folder", title: titleFor(session), session, request };
  }

  async function start(request: Request, replaceKey?: number) {
    error = "";
    try {
      const session = await api.openSession(request.left, request.right, request.options, request.mergeBase);
      const tab = sessionTab(session, request);
      const index = tabs.findIndex((t) => t.key === replaceKey);
      if (index >= 0) tabs[index] = tab;
      else tabs.push(tab);
      active = tab.key;
    } catch (e) {
      error = String(e);
    }
  }

  /** Releases what a comparison tab holds: its session, and the file tabs that share it. */
  function release(tab: SessionTab) {
    if (tab.kind === "folder") {
      api.closeSession(tab.session.id);
      tabs = tabs.filter((t) => !(t.kind === "text" && !t.request && t.session.id === tab.session.id));
    } else if (tab.request) {
      api.closeSession(tab.session.id);
    }
  }

  /**
   * Compares `request` in place of what `tab` shows. The old comparison stays
   * until the new one has opened, so a mistyped path costs nothing.
   */
  async function replace(tab: SessionTab, request: Request): Promise<boolean> {
    if (!(await confirmDiscard(unsaved(tab)))) return false;
    error = "";
    let session: SessionInfo;
    try {
      session = await api.openSession(request.left, request.right, request.options, request.mergeBase);
    } catch (e) {
      error = String(e);
      return false;
    }
    if (!tabs.some((t) => t.key === tab.key)) {
      api.closeSession(session.id);
      return false;
    }
    release(tab);
    const next = sessionTab(session, request);
    tabs[tabs.findIndex((t) => t.key === tab.key)] = next;
    active = next.key;
    return true;
  }

  /**
   * Compares `tab` with some sides replaced. A file opened from a folder comparison
   * becomes a comparison of its own, its other side being that file as before.
   */
  async function changeSides(tab: SessionTab, changes: Partial<Record<SideName, SourceSpec>>): Promise<boolean> {
    let request: Request;
    try {
      if (tab.request) {
        request = { ...tab.request, ...changes };
      } else {
        const side = (s: SideName) => (changes[s] ? Promise.resolve(changes[s]) : sideSpec(tab, s));
        const [left, right] = await Promise.all([side("left"), side("right")]);
        const parent = tabs.find((t) => t.kind === "folder" && t.session.id === tab.session.id) as SessionTab | undefined;
        const options = parent?.request?.options ?? { expandArchives: true, respectGitignore: true };
        request = { left, right, options, mergeBase: false };
      }
    } catch (e) {
      error = String(e);
      return false;
    }
    return replace(tab, request);
  }

  function sideSpec(tab: SessionTab, side: SideName): Promise<SourceSpec> {
    if (tab.request) return Promise.resolve(tab.request[side]);
    return api.sideSource(tab.session.id, side === "left", tab.kind === "text" ? tab.path : "");
  }

  function sideEdits(tab: SessionTab): [SideEdit, SideEdit] {
    const make = (side: SideName): SideEdit => ({
      spec: () => sideSpec(tab, side),
      apply: (spec) => changeSides(tab, { [side]: spec }),
      file: tab.kind === "text",
    });
    return [make("left"), make("right")];
  }

  /** File tabs with unsaved edits among those that closing `tab` closes. */
  function unsaved(tab: Tab): Tab[] {
    if (tab.kind === "text") return tab.dirty ? [tab] : [];
    if (tab.kind !== "folder") return [];
    return tabs.filter((t) => t.kind === "text" && t.dirty && !t.request && t.session.id === tab.session.id);
  }

  async function confirmDiscard(dirty: Tab[]): Promise<boolean> {
    if (!dirty.length) return true;
    const names = dirty.map((t) => t.title).join(", ");
    return ask(`Discard the unsaved changes in ${names}?`, { title: "Unsaved changes", kind: "warning", okLabel: "Discard", cancelLabel: "Keep editing" });
  }

  async function close(key: number) {
    const index = tabs.findIndex((t) => t.key === key);
    if (index < 0) return;
    const tab = tabs[index];
    if (!(await confirmDiscard(unsaved(tab)))) return;
    if (tab.kind !== "new") release(tab);
    tabs = tabs.filter((t) => t.key !== key);
    if (active === key) active = tabs[Math.min(index, tabs.length - 1)]?.key ?? 0;
    if (!tabs.length) newTab();
  }

  function openFile(session: SessionInfo, path: string) {
    const existing = tabs.find((t) => t.kind === "text" && t.session.id === session.id && t.path === path);
    if (existing) {
      active = existing.key;
      return;
    }
    add({ key: nextKey++, kind: "text", title: baseName(path), session, path,  });
  }

  function swap(tab: Extract<Tab, { kind: "folder" }>) {
    const r = tab.request;
    replace(tab, { ...r, left: r.right, right: r.left });
  }

  function dropSide(x: number, count: number): SideName | "both" {
    if (count >= 2) return "both";
    const r = mainEl!.getBoundingClientRect();
    return x < r.left + r.width / 2 ? "left" : "right";
  }

  function dropLabel(d: { side: SideName | "both"; count: number }): string {
    const tab = tabs.find((t) => t.key === active);
    if (d.side === "both") return d.count > 2 ? "Drop two files or folders to compare them" : "Compare these two";
    const side = d.side === "left" ? "left" : "right";
    return tab?.kind === "new" ? `Use as the ${side} side` : `Compare with this on the ${side}`;
  }

  /** Paths dropped on the window: on a comparison they replace one side (or both), on a new tab they fill the form. */
  async function dropped(paths: string[], side: SideName | "both") {
    const tab = tabs.find((t) => t.key === active);
    if (!tab || !paths.length || paths.length > 2) return;
    const spec = (path: string): SourceSpec => ({ type: "local", path });
    if (tab.kind === "new") {
      const form = pickers[tab.key];
      if (side === "both") form?.fill(paths[0], paths[1]);
      else form?.fill(side === "left" ? paths[0] : undefined, side === "right" ? paths[0] : undefined);
      return;
    }
    if (side === "both") await changeSides(tab, { left: spec(paths[0]), right: spec(paths[1]) });
    else await changeSides(tab, { [side]: spec(paths[0]) });
  }

  function onKey(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key === "w") {
      close(active);
      e.preventDefault();
    } else if ((e.ctrlKey || e.metaKey) && e.key === "t") {
      newTab();
      e.preventDefault();
    }
  }

  onMount(async () => {
    getCurrentWebview().onDragDropEvent((e) => {
      const p = e.payload;
      if (p.type === "leave") {
        drop = null;
        return;
      }
      const x = p.position.x / window.devicePixelRatio;
      if (p.type === "enter") drop = { side: dropSide(x, p.paths.length), count: p.paths.length };
      else if (p.type === "over" && drop) drop = { ...drop, side: dropSide(x, drop.count) };
      else if (p.type === "drop") {
        const side = dropSide(x, p.paths.length);
        drop = null;
        dropped(p.paths, side);
      }
    });
    getCurrentWindow().onCloseRequested(async (e) => {
      if (!(await confirmDiscard(tabs.filter((t) => t.kind === "text" && t.dirty)))) e.preventDefault();
    });
    const startup = await api.startupRequest().catch(() => null);
    if (startup) {
      await start({
        left: startup.left,
        right: startup.right,
        mergeBase: startup.mergeBase,
        options: { expandArchives: true, respectGitignore: true },
      });
    }
    if (!tabs.length) newTab();
  });
</script>

<svelte:window onkeydown={onKey} />

<div class="app">
  <nav class="tabs" aria-label="Comparisons">
    {#each tabs as tab (tab.key)}
      <div class="tab" class:on={tab.key === active}>
        <button class="label" onclick={() => (active = tab.key)} title={tab.title}>
          <span>{tab.kind === "folder" ? "📁" : tab.kind === "text" ? "📄" : "＋"}</span>{tab.title}{tab.kind === "text" && tab.dirty ? " ●" : ""}
        </button>
        <button class="x" onclick={() => close(tab.key)} title="Close (Ctrl+W)">×</button>
      </div>
    {/each}
    <button class="add" onclick={newTab} title="New comparison (Ctrl+T)">＋</button>
  </nav>

  {#if error}
    <div class="error">
      {error}
      <button onclick={() => (error = "")}>Dismiss</button>
    </div>
  {/if}

  <main bind:this={mainEl}>
    {#each tabs as tab (tab.key)}
      <section class="page" hidden={tab.key !== active}>
        {#if tab.kind === "new"}
          <NewCompare
            bind:this={pickers[tab.key]}
            onCompare={(left, right, options, mergeBase) => start({ left, right, options, mergeBase }, tab.key)}
          />
        {:else if tab.kind === "folder"}
          <FolderView
            session={tab.session}
            onOpenFile={(p) => openFile(tab.session, p)}
            onSwap={() => swap(tab)}
            edit={sideEdits(tab)}
          />
        {:else}
          <TextView session={tab.session} path={tab.path} onDirty={(d) => (tab.dirty = d)} edit={sideEdits(tab)} />
        {/if}
      </section>
    {/each}
    {#if drop}
      <div class="drop {drop.side}">
        <span>{dropLabel(drop)}</span>
      </div>
    {/if}
  </main>
</div>

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .tabs {
    display: flex;
    background: var(--chrome);
    border-bottom: 1px solid var(--line);
    overflow-x: auto;
    flex: none;
  }
  .tab {
    display: flex;
    align-items: center;
    border-right: 1px solid var(--line);
    border-top: 2px solid transparent;
    max-width: 280px;
  }
  .tab.on {
    background: var(--panel);
    border-top-color: var(--accent);
  }
  .tab button {
    background: none;
    border: none;
    border-radius: 0;
  }
  .label {
    display: flex;
    gap: 6px;
    padding: 6px 4px 6px 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--dim);
  }
  .tab.on .label {
    color: var(--text);
  }
  .x {
    color: var(--dim);
    padding: 4px 8px;
  }
  .x:hover {
    color: var(--text);
  }
  .add {
    background: none;
    border: none;
    color: var(--dim);
  }
  .error {
    display: flex;
    gap: 12px;
    align-items: center;
    justify-content: space-between;
    padding: 6px 12px;
    background: var(--row-changed);
    color: var(--text);
    border-bottom: 1px solid var(--line);
  }
  main {
    flex: 1;
    min-height: 0;
    position: relative;
  }
  .page {
    position: absolute;
    inset: 0;
  }
  .page[hidden] {
    display: none;
  }
  .drop {
    position: absolute;
    top: 0;
    bottom: 0;
    z-index: 50;
    display: flex;
    align-items: center;
    justify-content: center;
    pointer-events: none;
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    border: 2px dashed var(--accent);
    border-radius: 6px;
  }
  .drop.left {
    left: 0;
    right: 50%;
  }
  .drop.right {
    left: 50%;
    right: 0;
  }
  .drop.both {
    left: 0;
    right: 0;
  }
  .drop span {
    padding: 8px 14px;
    border-radius: 6px;
    background: var(--panel);
    border: 1px solid var(--accent);
    font-weight: 600;
  }
</style>
