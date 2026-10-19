<script lang="ts">
  import SidePicker, { toSpec, type PickerValue } from "./SidePicker.svelte";
  import { api, sourceName, sourceText, type SourceOptions, type SourceSpec } from "./api";
  import {
    comparisonKey,
    forgetComparison,
    pinComparison,
    recentComparisons,
    type SavedComparison,
  } from "./recent";

  let {
    onCompare,
  }: { onCompare: (left: SourceSpec, right: SourceSpec, options: SourceOptions, mergeBase: boolean) => void } =
    $props();

  const STORAGE_KEY = "parallax.newCompare";

  function load(): { left: PickerValue; right: PickerValue; options: SourceOptions; mergeBase: boolean } {
    const empty = (): PickerValue => ({ kind: "folder", path: "", rev: "", subpath: "" });
    const fallback = {
      left: empty(),
      right: empty(),
      options: { expandArchives: true, respectGitignore: true },
      mergeBase: false,
    };
    try {
      const saved = localStorage.getItem(STORAGE_KEY);
      return saved ? { ...fallback, ...JSON.parse(saved) } : fallback;
    } catch {
      return fallback;
    }
  }

  const initial = load();
  let left = $state(initial.left);
  let right = $state(initial.right);
  let options = $state(initial.options);
  let mergeBase = $state(initial.mergeBase);

  const bothGit = $derived(left.kind === "gitRev" && right.kind === "gitRev");
  const ready = $derived(left.path.trim() !== "" && right.path.trim() !== "");

  function swap() {
    [left, right] = [right, left];
  }

  async function picked(path: string, was: PickerValue): Promise<PickerValue> {
    const info = await api.inspectPath(path).catch(() => null);
    return { ...was, kind: info && !info.isDir ? "file" : "folder", path, rev: "", subpath: "" };
  }

  let recent = $state(recentComparisons());

  const title = (c: SavedComparison, depth: number) =>
    `${sourceName(c.left, depth)} ↔ ${sourceName(c.right, depth)}${c.mergeBase ? " (merge-base)" : ""}`;

  /** Titles for the recent list, naming parent folders where two would otherwise read the same. */
  const titles = $derived.by(() => {
    const depth = recent.map(() => 1);
    for (let round = 0; round < 4; round++) {
      const shown = recent.map((c, i) => title(c, depth[i]));
      let clash = false;
      shown.forEach((t, i) => {
        if (shown.indexOf(t) !== shown.lastIndexOf(t)) {
          depth[i]++;
          clash = true;
        }
      });
      if (!clash) break;
    }
    return recent.map((c, i) => title(c, depth[i]));
  });

  function ago(ms: number): string {
    const s = Math.max(0, (Date.now() - ms) / 1000);
    if (s < 60) return "just now";
    if (s < 3600) return `${Math.floor(s / 60)} min ago`;
    if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
    const days = Math.floor(s / 86400);
    return days === 1 ? "yesterday" : `${days} days ago`;
  }

  async function pickerFor(spec: SourceSpec): Promise<PickerValue> {
    switch (spec.type) {
      case "gitRev":
        return { kind: "gitRev", path: spec.repo, rev: spec.rev, subpath: spec.subpath ?? "" };
      case "gitWorktree":
        return { kind: "gitWorktree", path: spec.repo, rev: "", subpath: "" };
      case "local":
        return picked(spec.path, left);
    }
  }

  /** Puts a recent comparison into the form, to change it before comparing. */
  async function edit(c: SavedComparison) {
    left = await pickerFor(c.left);
    right = await pickerFor(c.right);
    options = { ...c.options };
    mergeBase = c.mergeBase;
  }

  function pin(c: SavedComparison) {
    pinComparison(comparisonKey(c), !c.pinned);
    recent = recentComparisons();
  }

  function forget(c: SavedComparison) {
    forgetComparison(comparisonKey(c));
    recent = recentComparisons();
  }

  /** Fills in dropped paths; with both sides given, compares right away. */
  export async function fill(leftPath?: string, rightPath?: string) {
    if (leftPath) left = await picked(leftPath, left);
    if (rightPath) right = await picked(rightPath, right);
    if (leftPath && rightPath) submit();
  }

  async function submit() {
    if (!ready) return;
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify({ left, right, options, mergeBase }));
    } catch {
      // Remembering the last comparison is a convenience only.
    }
    onCompare(await toSpec(left), await toSpec(right), { ...options }, bothGit && mergeBase);
  }
</script>

<div class="wrap">
  <form
    class="dialog"
    onsubmit={(e) => {
      e.preventDefault();
      submit();
    }}
  >
    <header>New comparison</header>
    <div class="sides">
      <SidePicker title="Left" bind:value={left} onEnter={submit} />
      <button type="button" class="swap" title="Swap sides" onclick={swap}>⇄</button>
      <SidePicker title="Right" bind:value={right} onEnter={submit} />
    </div>
    <div class="opts">
      <label title="Replace the left revision by its merge-base with the right one: shows only what the right side changed since it branched off">
        <input type="checkbox" bind:checked={mergeBase} disabled={!bothGit} /> Compare against merge-base
      </label>
      <label><input type="checkbox" bind:checked={options.expandArchives} /> Expand archives (zip, 7z, tar.*, jar …)</label>
      <label><input type="checkbox" bind:checked={options.respectGitignore} /> Respect .gitignore</label>
    </div>
    <footer>
      <span class="tip">
        Paths also accept <code>release.7z!/bin</code> and <code>~/src/repo@v1.4:src</code>.
      </span>
      <button type="submit" class="primary" disabled={!ready}>Compare</button>
    </footer>
  </form>

  {#if recent.length}
    <section class="recent" aria-label="Recent comparisons">
      <header>Recent comparisons</header>
      {#each recent as c, i (comparisonKey(c))}
        <div class="entry">
          <button
            class="open"
            onclick={() => onCompare(c.left, c.right, { ...c.options }, c.mergeBase)}
            title="Compare again with the same options"
          >
            <span class="names">{titles[i]}</span>
            <span class="paths mono">{sourceText(c.left)}  ↔  {sourceText(c.right)}</span>
          </button>
          <span class="when">{c.pinned ? "pinned" : ago(c.used)}</span>
          <button class="icon" class:pinned={c.pinned} onclick={() => pin(c)} title={c.pinned ? "Unpin" : "Pin: keep at the top"}>{c.pinned ? "★" : "☆"}</button>
          <button class="icon" onclick={() => edit(c)} title="Put into the form above to change it">✎</button>
          <button class="icon" onclick={() => forget(c)} title="Remove from the list">✕</button>
        </div>
      {/each}
    </section>
  {/if}
</div>

<style>
  .wrap {
    height: 100%;
    overflow: auto;
    padding: 32px 16px;
  }
  .dialog {
    max-width: 980px;
    margin: 0 auto;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.35);
  }
  header {
    padding: 10px 14px;
    border-bottom: 1px solid var(--line);
    font-weight: 600;
  }
  .sides {
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    gap: 12px;
    padding: 14px;
    align-items: start;
  }
  .swap {
    margin-top: 52px;
  }
  .opts {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 20px;
    padding: 0 14px 12px;
  }
  .opts label {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  footer {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
    padding: 10px 14px;
    border-top: 1px solid var(--line);
  }
  .tip {
    color: var(--dim);
    font-size: 12px;
  }
  code {
    font-family: var(--mono);
    background: var(--input);
    padding: 1px 4px;
    border-radius: 3px;
  }
  .recent {
    max-width: 980px;
    margin: 20px auto 0;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    overflow: hidden;
  }
  .recent header {
    padding: 8px 14px;
    border-bottom: 1px solid var(--line);
    font-size: 11px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--dim);
  }
  .entry {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px 2px 4px;
    border-bottom: 1px solid var(--line);
  }
  .entry:last-child {
    border-bottom: none;
  }
  .entry:hover {
    background: var(--panel-2);
  }
  .open {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    border: none;
    background: none;
    text-align: left;
    padding: 5px 10px;
  }
  .names {
    font-weight: 600;
  }
  .paths {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: pre;
    font-size: 11px;
    color: var(--dim);
  }
  .when {
    flex: none;
    font-size: 11px;
    color: var(--dim);
    margin-right: 6px;
  }
  .icon {
    flex: none;
    border: none;
    background: none;
    color: var(--dim);
    padding: 2px 6px;
  }
  .icon:hover,
  .icon.pinned {
    color: var(--accent);
  }
  @media (max-width: 760px) {
    .sides {
      grid-template-columns: 1fr;
    }
    .swap {
      margin: 0;
      justify-self: center;
    }
  }
</style>
