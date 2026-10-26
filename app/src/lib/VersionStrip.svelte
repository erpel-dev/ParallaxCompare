<script lang="ts">
  // One-click versions of a file in a Git repository: the working tree, the staged
  // file, HEAD and the upstream branch. Shown only inside a repository; the version
  // chip in the path bar has all the others. Shift+click switches both sides.
  import type { Version, Versions } from "./api";
  import type { SideEdit } from "./PathBar.svelte";
  import { ago, loadVersions, sameVersion, switchVersion } from "./versions";

  let { edit, refreshKey }: { edit: SideEdit; refreshKey: string } = $props();

  let versions = $state<Versions | null>(null);
  let busy = $state(false);

  $effect(() => {
    void refreshKey;
    let stale = false;
    edit
      .spec()
      .then((spec) => loadVersions(spec))
      .then((v) => !stale && (versions = v));
    return () => (stale = true);
  });

  const buttons = $derived.by(() => {
    const v = versions;
    if (!v) return [];
    const list: { label: string; title: string; version: Version; on: boolean }[] = [];
    const cur = v.current;
    if (v.hasWorktree) {
      list.push({ label: "Working tree", title: "The file on disk, with uncommitted changes", version: { type: "worktree" }, on: cur.type === "worktree" });
      list.push({ label: "Staged", title: "The staged file: what the next commit holds", version: { type: "staged" }, on: cur.type === "staged" });
    }
    const atHead = cur.type === "rev" && (cur.rev === "HEAD" || (!!v.head && cur.rev === v.head));
    list.push({ label: "HEAD", title: `The last commit${v.head ? ` on ${v.head}` : ""}${v.headShort ? ` (${v.headShort})` : ""}`, version: { type: "rev", rev: "HEAD" }, on: atHead });
    if (v.upstream) {
      const fetched = v.upstream.fetched ? `, fetched ${ago(v.upstream.fetched)}` : "";
      const version: Version = { type: "rev", rev: v.upstream.name };
      list.push({ label: v.upstream.name, title: `The upstream branch (${v.upstream.shortId}${fetched})`, version, on: sameVersion(cur, version) });
    }
    return list;
  });

  async function pick(version: Version, both: boolean) {
    if (busy) return;
    busy = true;
    try {
      await switchVersion(edit, version, both);
    } finally {
      busy = false;
    }
  }
</script>

{#if buttons.length}
  <span class="seg" role="group" aria-label="Versions of this file">
    {#each buttons as b (b.label)}
      <button class="mono" class:on={b.on} disabled={busy} title="{b.title} · Shift+click: both sides" onclick={(e) => !b.on && pick(b.version, e.shiftKey)}>{b.label}</button>
    {/each}
  </span>
{/if}

<style>
  .seg {
    display: inline-flex;
    flex: none;
    border: 1px solid var(--line);
    border-radius: 4px;
    overflow: hidden;
    vertical-align: middle;
  }
  button {
    border: none;
    border-left: 1px solid var(--line);
    border-radius: 0;
    background: var(--input);
    color: var(--fg);
    padding: 0 7px;
    font-size: 11px;
    line-height: 16px;
    white-space: nowrap;
  }
  button:first-child {
    border-left: none;
  }
  button.on {
    background: var(--accent);
    color: #fff;
  }
  button:hover:not(.on):not(:disabled) {
    background: var(--sel);
  }
</style>
