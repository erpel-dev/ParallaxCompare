<script lang="ts" module>
  export type PickerKind = "folder" | "file" | "gitRev" | "gitWorktree";

  export interface PickerValue {
    kind: PickerKind;
    path: string;
    rev: string;
    subpath: string;
  }

  export function toSpec(v: PickerValue): import("./api").SourceSpec {
    switch (v.kind) {
      case "folder":
      case "file":
        return { type: "local", path: v.path.trim() };
      case "gitRev":
        return { type: "gitRev", repo: v.path.trim(), rev: v.rev.trim() || "HEAD", subpath: v.subpath.trim() };
      case "gitWorktree":
        return { type: "gitWorktree", repo: v.path.trim() };
    }
  }
</script>

<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { api, type RefList } from "./api";
  import PathInput from "./PathInput.svelte";

  let {
    title,
    value = $bindable(),
    onEnter,
  }: { title: string; value: PickerValue; onEnter?: () => void } = $props();

  const kinds: { id: PickerKind; label: string }[] = [
    { id: "folder", label: "Folder" },
    { id: "file", label: "File / Archive" },
    { id: "gitRev", label: "Git ref" },
    { id: "gitWorktree", label: "Worktree" },
  ];

  let refs = $state<RefList | null>(null);
  let refsError = $state("");
  let filter = $state("");
  let gitHint = $state<string | null>(null);

  async function browse() {
    const wantsFile = value.kind === "file";
    const picked = await open({
      directory: !wantsFile,
      multiple: false,
      title: wantsFile ? "Choose a file or archive" : "Choose a folder",
    });
    if (typeof picked === "string") value.path = picked;
  }

  // Load refs whenever a Git source points at a new path.
  let refsFor = "";
  $effect(() => {
    const path = value.path.trim();
    if (value.kind !== "gitRev" || !path || path === refsFor) return;
    const timer = setTimeout(async () => {
      refsFor = path;
      try {
        refs = await api.gitRefs(path);
        refsError = "";
        if (!value.rev && refs.head) value.rev = refs.head;
      } catch (e) {
        refs = null;
        refsError = String(e);
      }
    }, 300);
    return () => clearTimeout(timer);
  });

  // Offer the Git modes when a plain folder turns out to be a repository.
  $effect(() => {
    const path = value.path.trim();
    if (value.kind !== "folder" || !path) {
      gitHint = null;
      return;
    }
    const timer = setTimeout(async () => {
      try {
        const info = await api.inspectPath(path);
        gitHint = info.isDir ? info.gitRoot : null;
      } catch {
        gitHint = null;
      }
    }, 400);
    return () => clearTimeout(timer);
  });

  const matches = (name: string) => !filter || name.toLowerCase().includes(filter.toLowerCase());
</script>

<div class="picker">
  <h4>{title}</h4>
  <div class="seg" role="tablist">
    {#each kinds as k (k.id)}
      <button type="button" role="tab" aria-selected={value.kind === k.id} class:on={value.kind === k.id} onclick={() => (value.kind = k.id)}>
        {k.label}
      </button>
    {/each}
  </div>

  <div class="row">
    <PathInput
      bind:value={value.path}
      label="{title} path"
      placeholder={value.kind === "gitRev" || value.kind === "gitWorktree" ? "Repository folder" : value.kind === "file" ? "File or archive (a.zip!/sub/dir also works)" : "Folder"}
      {onEnter}
    />
    <button type="button" onclick={browse}>Browse…</button>
  </div>

  {#if value.kind === "folder" && gitHint}
    <p class="hint">
      This folder is in a Git repository.
      <button type="button" class="link" onclick={() => { value.kind = "gitRev"; value.path = gitHint!; }}>Compare a branch instead</button>
    </p>
  {/if}

  {#if value.kind === "gitRev"}
    <div class="row">
      <input type="text" class="mono grow" placeholder="Branch, tag, commit or HEAD~2" bind:value={value.rev} spellcheck="false" />
      <input type="text" class="mono sub" placeholder="sub-folder (optional)" bind:value={value.subpath} spellcheck="false" />
    </div>
    {#if refsError}
      <p class="hint error">{refsError}</p>
    {:else if refs}
      <input type="text" class="filter" placeholder="Filter refs…" bind:value={filter} />
      <div class="reflist">
        {#each [["Branches", refs.branches], ["Remote branches", refs.remotes], ["Tags", refs.tags]] as const as [label, list] (label)}
          {@const shown = list.filter((r) => matches(r.name))}
          {#if shown.length}
            <div class="grp">{label}</div>
            {#each shown as r (r.name)}
              <button type="button" class="ref" class:on={value.rev === r.name} onclick={() => (value.rev = r.name)}>
                <span>{label === "Tags" ? "🏷" : "⎇"} {r.name}</span><span class="mono dim">{r.shortId}</span>
              </button>
            {/each}
          {/if}
        {/each}
        {#if refs.recentCommits.some((c) => matches(c.summary))}
          <div class="grp">Recent commits</div>
          {#each refs.recentCommits.filter((c) => matches(c.summary)) as c (c.id)}
            <button type="button" class="ref" class:on={value.rev === c.shortId} onclick={() => (value.rev = c.shortId)}>
              <span class="ellipsis">● {c.summary}</span><span class="mono dim">{c.shortId}</span>
            </button>
          {/each}
        {/if}
      </div>
    {/if}
  {/if}
</div>

<style>
  .picker {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
  }
  h4 {
    margin: 0;
    font-size: 11px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--dim);
  }
  .seg {
    display: flex;
  }
  .seg button {
    flex: 1;
    border-radius: 0;
    margin-right: -1px;
  }
  .seg button:first-child {
    border-radius: 4px 0 0 4px;
  }
  .seg button:last-child {
    border-radius: 0 4px 4px 0;
  }
  .row {
    display: flex;
    gap: 6px;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .sub {
    width: 38%;
  }
  .hint {
    margin: 0;
    color: var(--dim);
    font-size: 12px;
  }
  .hint.error {
    color: var(--error);
  }
  .link {
    background: none;
    border: none;
    color: var(--accent);
    padding: 0;
    text-decoration: underline;
  }
  .reflist {
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--input);
    max-height: 260px;
    overflow: auto;
  }
  .grp {
    position: sticky;
    top: 0;
    padding: 3px 8px;
    font-size: 11px;
    text-transform: uppercase;
    color: var(--dim);
    background: var(--panel-2);
  }
  .ref {
    display: flex;
    width: 100%;
    justify-content: space-between;
    gap: 8px;
    border: none;
    border-radius: 0;
    background: none;
    padding: 3px 8px;
    text-align: left;
  }
  .ref.on {
    background: var(--sel);
  }
  .ref:hover {
    background: var(--panel);
  }
  .ellipsis {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .dim {
    color: var(--dim);
    font-size: 11px;
  }
</style>
