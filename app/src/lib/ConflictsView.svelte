<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { api, type ConflictKind, type FileMode, type RepoConflicts } from "./api";
  import { notify } from "./toast.svelte";
  import { syntaxMerge } from "./syntaxMerge.svelte";

  let {
    dir,
    onOpenMerge,
    onLoaded,
  }: {
    /** A folder in the repository. */
    dir: string;
    onOpenMerge: (workdir: string, path: string) => void;
    onLoaded?: (repo: RepoConflicts) => void;
  } = $props();

  // Raw: replaced as a whole on every refresh.
  let repo = $state.raw<RepoConflicts | null>(null);
  let error = $state("");
  let busy = $state(false);
  /** Files resolved here but not staged yet: how, whether staging removes them, and
   * the kind of file to record when a whole version was taken. */
  let resolved = $state<Record<string, { how: string; remove: boolean; mode?: FileMode | null }>>({});
  let unlisten: UnlistenFn | undefined;
  /** Conflicts the syntax-aware merge leaves in each text file (0: it solves the file), once checked. */
  let syntaxLeft = $state<Record<string, number>>({});
  /** The engine `syntaxLeft` comes from; shown only while it is still the one chosen. */
  let checkedWith = $state("");
  const syntaxOn = $derived(syntaxMerge.enabled && !!checkedWith && syntaxMerge.found?.name === checkedWith);
  let checking = $state(false);
  let checkRun = 0;
  const solvable = $derived(
    syntaxOn ? (repo?.files ?? []).filter((f) => syntaxLeft[f.path] === 0 && !resolved[f.path]).map((f) => f.path) : [],
  );

  const kindLabel: Record<ConflictKind, string> = {
    bothModified: "both modified",
    bothAdded: "both added",
    deletedByUs: "deleted locally",
    deletedByThem: "deleted remotely",
    addedByUs: "added locally",
    addedByThem: "added remotely",
    bothDeleted: "deleted on both",
  };

  /** Whether a side has no version of the file in this kind of conflict. */
  function missing(kind: ConflictKind, remote: boolean): boolean {
    if (kind === "bothDeleted") return true;
    return remote ? kind === "deletedByThem" || kind === "addedByUs" : kind === "deletedByUs" || kind === "addedByThem";
  }

  const ready = $derived(Object.keys(resolved).filter((p) => repo?.files.some((f) => f.path === p)));

  onMount(async () => {
    unlisten = await listen<{ workdir: string; path: string; clean: boolean }>("merge-saved", (e) => {
      if (!repo || e.payload.workdir !== repo.workdir || !repo.files.some((f) => f.path === e.payload.path)) return;
      if (e.payload.clean) resolved[e.payload.path] = { how: "merged", remove: false };
      else delete resolved[e.payload.path];
    });
    refresh();
  });

  onDestroy(() => unlisten?.());

  // A different engine chosen (in a merge tab) checks the files again.
  let checkedFor = "";
  $effect(() => {
    const key = `${syntaxMerge.engine}\n${syntaxMerge.path}`;
    if (repo && checkedFor && key !== checkedFor) checkSyntax(repo);
    checkedFor = key;
  });

  async function refresh() {
    try {
      repo = await api.repoConflicts(dir);
      error = "";
      onLoaded?.(repo);
      checkSyntax(repo);
    } catch (e) {
      error = String(e);
    }
  }

  async function takeSide(path: string, remote: boolean) {
    if (!repo) return;
    try {
      const taken = await api.takeConflictSide(repo.workdir, path, remote);
      const side = remote ? "remote" : "local";
      resolved[path] = { how: taken.deleted ? `${side} deletion kept` : `took ${side}`, remove: taken.deleted, mode: taken.mode };
    } catch (e) {
      notify(String(e));
    }
  }

  /** Asks the syntax-aware merge, file by file, which conflicts it can solve; writes nothing. */
  async function checkSyntax(state: RepoConflicts) {
    const run = ++checkRun;
    syntaxLeft = {};
    checkedWith = "";
    const engine = await syntaxMerge.detect();
    if (!engine || run !== checkRun) return;
    checkedWith = engine.name;
    checking = true;
    try {
      for (const f of state.files.filter((f) => f.text)) {
        const left = await api
          .structuredMergeConflict(state.workdir, f.path, syntaxMerge.engine, syntaxMerge.program, false)
          .catch(() => null);
        if (run !== checkRun) return;
        if (left !== null) syntaxLeft[f.path] = left;
      }
    } finally {
      if (run === checkRun) checking = false;
    }
  }

  /** Writes the syntax-aware results into the work tree; staging stays a separate step. */
  async function solve(paths: string[]) {
    if (!repo) return;
    for (const path of paths) {
      try {
        const left = await api.structuredMergeConflict(repo.workdir, path, syntaxMerge.engine, syntaxMerge.program, true);
        const by = syntaxMerge.found?.name ?? "syntax";
        if (left === 0) resolved[path] = { how: `solved by ${by}, please review`, remove: false };
        else if (left === null) notify(`${by} does not merge ${path}.`, "info");
        else notify(`${by} left ${left} conflict${left === 1 ? "" : "s"} in ${path}.`, "info");
      } catch (e) {
        notify(String(e));
      }
    }
  }

  function markResolved(path: string) {
    resolved[path] = { how: "as in the work tree", remove: false };
  }

  async function stage() {
    if (!repo || !ready.length) return;
    busy = true;
    try {
      await api.stageConflicts(repo.workdir, ready.map((p) => [p, resolved[p].remove, resolved[p].mode ?? null]));
      notify(`Staged ${ready.length} file${ready.length === 1 ? "" : "s"}.`, "info");
      resolved = {};
      await refresh();
    } catch (e) {
      notify(String(e));
    } finally {
      busy = false;
    }
  }
</script>

<div class="conflicts">
  <div class="toolbar">
    <div class="what">
      <strong>{repo?.summary ?? "Conflicts"}</strong>
      {#if repo}<span class="path" title={repo.workdir}>{repo.workdir}</span>{/if}
    </div>
    <span class="grow"></span>
    {#if checking}<span class="checking">✦ Merging by syntax…</span>{/if}
    {#if solvable.length}
      <button class="sa" onclick={() => solve(solvable)} title="Write the syntax-aware result into the work tree for each file it solves completely; nothing is staged">
        ✦ Solve {solvable.length} by syntax
      </button>
    {/if}
    <button onclick={refresh} title="Read the conflicts from the index again">⟳ Refresh</button>
    <button class="primary" onclick={stage} disabled={busy || !ready.length} title="git add (or git rm) the files resolved here">
      Stage {ready.length || ""} resolved file{ready.length === 1 ? "" : "s"}
    </button>
  </div>

  {#if error}<div class="banner err">{error}</div>{/if}

  {#if repo}
    <div class="labels">
      <span><span class="badge local">LOCAL</span>{repo.labels.local}</span>
      <span><span class="badge base">BASE</span>{repo.labels.base}</span>
      <span><span class="badge remote">REMOTE</span>{repo.labels.remote}</span>
    </div>
    {#if !repo.files.length}
      <div class="empty">
        <strong>No conflicts left.</strong>
        <span>Every file is resolved and staged. Continue the operation with Git, for example <code>git commit</code>, <code>git rebase --continue</code> or <code>git cherry-pick --continue</code>.</span>
      </div>
    {:else}
      <table>
        <thead>
          <tr><th>File</th><th>Conflict</th><th>State</th><th class="actions">Resolve</th></tr>
        </thead>
        <tbody>
          {#each repo.files as f (f.path)}
            {@const done = resolved[f.path]}
            <tr class:done={!!done}>
              <td class="file" title={f.path}>
                {#if f.text}
                  <button class="link" onclick={() => onOpenMerge(repo!.workdir, f.path)} title="Open the 3-way merge">{f.path}</button>
                {:else}
                  {f.path}
                {/if}
              </td>
              <td><span class="chip {f.kind}">{kindLabel[f.kind]}</span>{#if f.special === "symlink"}<span class="chip" title="A symbolic link: take one version as a whole">link</span>{:else if f.special === "gitlink"}<span class="chip" title="A submodule: resolve it with Git">submodule</span>{/if}{#if !f.text && !f.special && (f.kind === "bothModified" || f.kind === "bothAdded")}<span class="chip">binary</span>{/if}{#if syntaxOn && syntaxLeft[f.path] === 0}<span class="chip sa" title={`${syntaxMerge.found?.name ?? "The syntax-aware merge"} merges this file without conflicts`}>✦ solvable by syntax</span>{/if}</td>
              <td class="state">{done ? `✓ ${done.how}` : "unresolved"}</td>
              <td class="actions">
                {#if syntaxOn && syntaxLeft[f.path] === 0 && !done}<button class="sa" onclick={() => solve([f.path])} title="Write the syntax-aware result into the work tree">✦ Solve</button>{/if}
                {#if f.text}<button onclick={() => onOpenMerge(repo!.workdir, f.path)}>Open merge</button>{/if}
                <button onclick={() => takeSide(f.path, false)} title={missing(f.kind, false) ? "Delete the file, as local did" : "Use the local version of the whole file"}>
                  {missing(f.kind, false) ? "Delete (local)" : "Keep local"}
                </button>
                <button onclick={() => takeSide(f.path, true)} title={missing(f.kind, true) ? "Delete the file, as remote did" : "Use the remote version of the whole file"}>
                  {missing(f.kind, true) ? "Delete (remote)" : "Take remote"}
                </button>
                {#if !done}
                  <button class="link" onclick={() => markResolved(f.path)} title="The file in the work tree is resolved already">Mark resolved</button>
                {:else}
                  <button class="link" onclick={() => delete resolved[f.path]} title="Leave the file out of staging; the work tree keeps what was written">Unmark</button>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
      <p class="hint">
        Open a text file to merge it line by line; saving it without conflict markers marks it resolved here. Nothing is staged until you choose
        <em>Stage resolved files</em>.
      </p>
    {/if}
  {:else if !error}
    <div class="banner">Reading conflicts…</div>
  {/if}
</div>

<style>
  .conflicts {
    height: 100%;
    overflow: auto;
    background: var(--panel);
    display: flex;
    flex-direction: column;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    background: var(--chrome);
    border-bottom: 1px solid var(--line);
  }
  .what {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .what .path {
    color: var(--dim);
    font-size: 11px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .grow {
    flex: 1;
  }
  .labels {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 20px;
    padding: 10px 12px;
    border-bottom: 1px solid var(--line);
    font-size: 12px;
  }
  .badge {
    font-size: 10px;
    font-weight: 700;
    padding: 1px 6px;
    border-radius: 3px;
    margin-right: 6px;
    color: #fff;
  }
  .badge.local {
    background: var(--accent);
  }
  .badge.base {
    background: var(--similar);
  }
  .badge.remote {
    background: #a27ce8;
  }
  table {
    border-collapse: collapse;
    width: 100%;
  }
  th,
  td {
    text-align: left;
    padding: 6px 12px;
    border-bottom: 1px solid var(--line);
    vertical-align: middle;
  }
  th {
    font-size: 11px;
    color: var(--dim);
    font-weight: 600;
  }
  td.file {
    font-family: var(--mono);
    max-width: 420px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .actions {
    text-align: right;
    white-space: nowrap;
  }
  .actions button {
    margin-left: 4px;
  }
  tr.done td.state {
    color: var(--accent);
  }
  td.state {
    color: var(--dim);
    white-space: nowrap;
  }
  .chip {
    display: inline-block;
    font-size: 11px;
    padding: 1px 7px;
    border-radius: 9px;
    border: 1px solid var(--line);
    margin-right: 4px;
    white-space: nowrap;
  }
  .chip.sa {
    border-color: #1f9e7a;
    color: #1f9e7a;
  }
  button.sa {
    color: #1f9e7a;
    border-color: #1f9e7a;
  }
  .checking {
    color: var(--dim);
    font-size: 12px;
  }
  .chip.deletedByUs,
  .chip.deletedByThem,
  .chip.bothDeleted {
    border-color: var(--diff);
  }
  .link {
    border: none;
    background: none;
    color: var(--accent);
    padding: 0;
    font: inherit;
  }
  .banner {
    padding: 8px 12px;
    border-bottom: 1px solid var(--line);
  }
  .banner.err {
    color: var(--diff);
  }
  .empty {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 32px 20px;
    align-items: center;
    text-align: center;
  }
  .hint {
    color: var(--dim);
    font-size: 12px;
    padding: 4px 12px;
  }
</style>
