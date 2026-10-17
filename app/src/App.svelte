<script lang="ts">
  import { onMount } from "svelte";
  import FolderView from "./lib/FolderView.svelte";
  import NewCompare from "./lib/NewCompare.svelte";
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
    | { key: number; kind: "text"; title: string; session: SessionInfo; path: string; ownsSession: boolean };

  let tabs = $state<Tab[]>([]);
  let active = $state(0);
  let nextKey = 1;
  let error = $state("");

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

  async function start(request: Request, replaceKey?: number) {
    error = "";
    try {
      const session = await api.openSession(request.left, request.right, request.options, request.mergeBase);
      const key = nextKey++;
      const tab: Tab = session.isFile
        ? { key, kind: "text", title: titleFor(session), session, path: "", ownsSession: true }
        : { key, kind: "folder", title: titleFor(session), session, request };
      const index = tabs.findIndex((t) => t.key === replaceKey);
      if (index >= 0) tabs[index] = tab;
      else tabs.push(tab);
      active = key;
    } catch (e) {
      error = String(e);
    }
  }

  function close(key: number) {
    const index = tabs.findIndex((t) => t.key === key);
    if (index < 0) return;
    const tab = tabs[index];
    if (tab.kind === "folder") {
      api.closeSession(tab.session.id);
      // File tabs opened from this comparison share its session.
      tabs = tabs.filter((t) => !(t.kind === "text" && !t.ownsSession && t.session.id === tab.session.id));
    } else if (tab.kind === "text" && tab.ownsSession) {
      api.closeSession(tab.session.id);
    }
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
    add({ key: nextKey++, kind: "text", title: baseName(path), session, path, ownsSession: false });
  }

  function swap(tab: Extract<Tab, { kind: "folder" }>) {
    const r = tab.request;
    api.closeSession(tab.session.id);
    start({ ...r, left: r.right, right: r.left }, tab.key);
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
          <span>{tab.kind === "folder" ? "📁" : tab.kind === "text" ? "📄" : "＋"}</span>{tab.title}
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

  <main>
    {#each tabs as tab (tab.key)}
      <section class="page" hidden={tab.key !== active}>
        {#if tab.kind === "new"}
          <NewCompare onCompare={(left, right, options, mergeBase) => start({ left, right, options, mergeBase }, tab.key)} />
        {:else if tab.kind === "folder"}
          <FolderView session={tab.session} onOpenFile={(p) => openFile(tab.session, p)} onSwap={() => swap(tab)} />
        {:else}
          <TextView session={tab.session} path={tab.path} />
        {/if}
      </section>
    {/each}
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
</style>
