<script lang="ts">
  // The version a side shows when it lies in a Git repository (working tree,
  // staged, a branch, tag or commit), and a list to switch it to another one.
  // Shown only inside a repository. Alt+V opens it for the active side.
  import { tick } from "svelte";
  import { api, type FileHistory, type Version, type Versions } from "./api";
  import type { SideEdit } from "./PathBar.svelte";
  import { ago, loadVersions, revOf, sameVersion, switchVersion, versionLabel } from "./versions";
  import { notify } from "./toast.svelte";

  let {
    edit,
    refreshKey,
    commit = null,
    active = false,
    shown = $bindable(false),
  }: {
    edit: SideEdit;
    /** Changes whenever the side does, to read its versions again. */
    refreshKey: string;
    /** The short commit id the side shows, for a revision. */
    commit?: string | null;
    /** The side has the focus, so Alt+V opens this list. */
    active?: boolean;
    /** Whether the chip shows: the side is in a local Git repository. */
    shown?: boolean;
  } = $props();

  interface Item {
    key: string;
    group: string;
    label: string;
    detail?: string;
    version: Version;
  }

  let versions = $state<Versions | null>(null);
  let open = $state(false);
  let query = $state("");
  let highlight = $state(0);
  let busy = $state(false);
  let changed = $state<number | null | undefined>(undefined);
  let history = $state<FileHistory | null>(null);
  let other = $state<Versions | null>(null);
  let base = $state<string | null>(null);
  let root = $state<HTMLSpanElement>();
  let search = $state<HTMLInputElement>();
  let list = $state<HTMLDivElement>();

  $effect(() => {
    void refreshKey;
    let stale = false;
    edit
      .spec()
      .then((spec) => loadVersions(spec))
      .then((v) => {
        if (stale) return;
        versions = v;
        shown = !!v;
      });
    return () => (stale = true);
  });

  /** Opens the list, and reads what it shows besides the refs. */
  export async function openList() {
    if (!versions || open) return;
    const v = versions;
    open = true;
    query = "";
    highlight = 0;
    changed = undefined;
    history = null;
    other = null;
    base = null;
    await tick();
    search?.focus();
    if (v.hasWorktree) api.changedFiles(v.repo, v.path || null).then((n) => (changed = n), () => (changed = null));
    if (edit.file) {
      edit.spec().then(api.fileHistory).then((h) => (history = h), () => {});
    }
    edit.other?.()
      .then((spec) => loadVersions(spec))
      .then((o) => {
        if (!o || o.repo !== v.repo) return;
        other = o;
        if (!sameVersion(o.current, v.current)) {
          api.gitMergeBase(v.repo, revOf(v.current), revOf(o.current)).then((b) => (base = b), () => {});
        }
      });
  }

  function close() {
    open = false;
  }

  const items = $derived.by((): Item[] => {
    const v = versions;
    if (!v) return [];
    const q = query.trim().toLowerCase();
    const hit = (...texts: (string | null | undefined)[]) => !q || texts.some((t) => t?.toLowerCase().includes(q));
    const out: Item[] = [];
    const add = (item: Item) => hit(item.label, item.detail) && out.push(item);
    const repo = "This repository";
    if (v.hasWorktree) {
      const what = edit.file ? (changed ? "changed" : changed === 0 ? "unchanged" : "") : changed ? `${changed} changed file${changed === 1 ? "" : "s"}` : changed === 0 ? "no changes" : "";
      add({ key: "wt", group: repo, label: "working tree", detail: what || "the files on disk", version: { type: "worktree" } });
      add({ key: "staged", group: repo, label: "staged", detail: "what the next commit holds", version: { type: "staged" } });
    }
    add({
      key: "head",
      group: repo,
      label: v.head ? `HEAD · ${v.head}` : "HEAD (detached)",
      detail: v.headShort ?? "",
      version: { type: "rev", rev: "HEAD" },
    });
    if (v.upstream) {
      const fetched = v.upstream.fetched ? `fetched ${ago(v.upstream.fetched)}` : "not fetched yet";
      add({ key: "up", group: repo, label: v.upstream.name, detail: `upstream · ${fetched}`, version: { type: "rev", rev: v.upstream.name } });
    }
    const refs = (group: string, list: typeof v.branches, few: number) => {
      const matching = list.filter((r) => hit(r.name));
      for (const r of matching.slice(0, q ? 40 : few)) {
        add({ key: `${group}:${r.name}`, group, label: r.name, detail: r.time ? ago(r.time) : r.shortId, version: { type: "rev", rev: r.name } });
      }
    };
    refs("Branches", v.branches.filter((b) => b.name !== v.head), 6);
    refs("Tags", v.tags, 4);
    refs("Remote branches", v.remotes.filter((r) => r.name !== v.upstream?.name), 4);
    if (history) {
      for (const c of history.commits.filter((c) => hit(c.shortId, c.summary)).slice(0, q ? 40 : 15)) {
        out.push({ key: `c:${c.id}`, group: "Commits that changed this file", label: `${c.shortId} ${c.summary}`, detail: ago(c.time), version: { type: "rev", rev: c.shortId } });
      }
    }
    if (other && !sameVersion(other.current, v.current)) {
      add({ key: "same", group: "From the other side", label: `same as the other side (${versionLabel(other.current)})`, version: other.current });
      if (base) add({ key: "base", group: "From the other side", label: "merge base with the other side", detail: base, version: { type: "rev", rev: base } });
    }
    // Anything Git understands: a commit id, HEAD~3, a tag not listed above.
    if (q && !out.some((i) => i.label.toLowerCase() === q)) {
      out.push({ key: "typed", group: "Revision", label: `go to “${query.trim()}”`, version: { type: "rev", rev: query.trim() } });
    }
    return out;
  });

  async function pick(item: Item | undefined, both: boolean) {
    if (!item || busy) return;
    busy = true;
    try {
      if (await switchVersion(edit, item.version, both)) close();
    } finally {
      busy = false;
    }
  }

  async function fetchRemote() {
    if (!versions || busy) return;
    busy = true;
    try {
      await api.gitFetch(versions.repo);
      versions = await loadVersions(await edit.spec(), true);
      notify("Fetched", "info");
    } catch (e) {
      notify(String(e));
    } finally {
      busy = false;
    }
  }

  async function onKey(e: KeyboardEvent) {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (!items.length) return;
      highlight = (highlight + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
      await tick();
      list?.querySelector(".on")?.scrollIntoView({ block: "nearest" });
    } else if (e.key === "Enter") {
      e.preventDefault();
      pick(items[highlight], e.shiftKey);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      close();
    }
  }

  function onWindowKey(e: KeyboardEvent) {
    if (active && e.altKey && !e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "v" && root?.offsetParent) {
      e.preventDefault();
      openList();
    }
  }

  function outside(e: MouseEvent) {
    if (open && root && !root.contains(e.target as Node)) close();
  }

  const isCurrent = (item: Item) => !!versions && sameVersion(item.version, versions.current);
</script>

<svelte:window onkeydown={onWindowKey} onmousedown={outside} />

{#if versions}
  <span class="chipwrap" bind:this={root}>
    <button
      class="chip mono"
      class:on={open}
      class:wt={versions.current.type !== "rev"}
      onclick={() => (open ? close() : openList())}
      title="Switch to another version of this side (Alt+V){commit ? ` · ${commit}` : ''}"
    >
      <span class="name">⎇ {versionLabel(versions.current)}</span>{#if commit && versions.current.type === "rev" && commit !== versionLabel(versions.current)}<span class="commit"> · {commit}</span>{/if}<span class="car">▾</span>
    </button>
    {#if open}
      <div class="pop" role="dialog" aria-label="Versions">
        <input
          class="search mono"
          bind:this={search}
          bind:value={query}
          oninput={() => (highlight = 0)}
          onkeydown={onKey}
          placeholder="Filter, or type a commit, tag or HEAD~2"
          aria-label="Filter versions"
          spellcheck="false"
        />
        <div class="list" bind:this={list} role="listbox">
          {#each items as item, i (item.key)}
            {#if i === 0 || items[i - 1].group !== item.group}<div class="grp">{item.group}</div>{/if}
            <!-- Mousedown, not click: the filter field keeps the focus. -->
            <div
              class="opt"
              class:on={i === highlight}
              class:cur={isCurrent(item)}
              role="option"
              aria-selected={i === highlight}
              tabindex="-1"
              onmousedown={(e) => {
                e.preventDefault();
                pick(item, e.shiftKey);
              }}
              onmouseenter={() => (highlight = i)}
            >
              <span class="n mono">{item.label}</span>
              {#if item.detail}<span class="w">{item.detail}</span>{/if}
            </div>
          {:else}
            <div class="empty">No version matches.</div>
          {/each}
        </div>
        <div class="foot">
          <span><kbd>Enter</kbd> switch · <kbd>Shift+Enter</kbd> both sides</span>
          {#if versions.upstream || versions.remotes.length}
            <button class="link" onclick={fetchRemote} disabled={busy} title="git fetch: update the remote branches">Fetch</button>
          {/if}
        </div>
      </div>
    {/if}
  </span>
{/if}

<style>
  .chipwrap {
    position: relative;
    flex: 0 1 auto;
    min-width: 3em;
    max-width: 45%;
    display: flex;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
    max-width: 100%;
    border: 1px solid var(--line);
    background: var(--input);
    border-radius: 11px;
    padding: 0 8px;
    font-size: 11.5px;
    color: #e6a974;
    white-space: nowrap;
    cursor: pointer;
  }
  .chip.wt {
    color: #5fb37a;
  }
  .chip:hover,
  .chip.on {
    border-color: var(--accent);
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .commit {
    flex: none;
    white-space: pre;
  }
  .car {
    flex: none;
    margin-left: 4px;
    color: var(--dim);
  }
  .pop {
    position: absolute;
    top: calc(100% + 4px);
    right: 0;
    z-index: 30;
    width: 380px;
    max-width: 80vw;
    display: flex;
    flex-direction: column;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 6px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.35);
  }
  .search {
    margin: 8px;
    padding: 3px 8px;
    border: 1px solid var(--accent);
    border-radius: 4px;
    background: var(--input);
    color: inherit;
    font-size: 12px;
  }
  .list {
    max-height: 360px;
    overflow: auto;
    padding-bottom: 4px;
  }
  .grp {
    padding: 6px 10px 2px;
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--dim);
  }
  .opt {
    display: flex;
    gap: 10px;
    align-items: baseline;
    padding: 3px 10px;
    cursor: default;
  }
  .opt.on {
    background: var(--sel);
  }
  .opt .n {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
  }
  .opt.cur .n::before {
    content: "✓ ";
    color: var(--accent);
  }
  .opt .w {
    flex: none;
    color: var(--dim);
    font-size: 11.5px;
    white-space: nowrap;
  }
  .empty {
    padding: 6px 10px;
    color: var(--dim);
  }
  .foot {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    border-top: 1px solid var(--line);
    color: var(--dim);
    font-size: 11.5px;
  }
  kbd {
    font-family: var(--mono);
    font-size: 10.5px;
    border: 1px solid var(--line);
    border-bottom-width: 2px;
    border-radius: 3px;
    padding: 0 3px;
  }
  .link {
    border: none;
    background: none;
    color: var(--accent);
    padding: 0;
    cursor: pointer;
  }
</style>
