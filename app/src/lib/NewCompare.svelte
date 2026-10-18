<script lang="ts">
  import SidePicker, { toSpec, type PickerValue } from "./SidePicker.svelte";
  import { api, type SourceOptions, type SourceSpec } from "./api";

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

  /** Fills in dropped paths; with both sides given, compares right away. */
  export async function fill(leftPath?: string, rightPath?: string) {
    if (leftPath) left = await picked(leftPath, left);
    if (rightPath) right = await picked(rightPath, right);
    if (leftPath && rightPath) submit();
  }

  function submit() {
    if (!ready) return;
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify({ left, right, options, mergeBase }));
    } catch {
      // Remembering the last comparison is a convenience only.
    }
    onCompare(toSpec(left), toSpec(right), { ...options }, bothGit && mergeBase);
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
