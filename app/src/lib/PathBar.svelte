<script lang="ts" module>
  import type { SourceSpec } from "./api";

  /** Changing one side of a comparison from its path bar. */
  export interface SideEdit {
    /** The side as it is compared now (for a file opened from a folder comparison: that file). */
    spec: () => Promise<SourceSpec>;
    /** Compares against `spec` instead; resolves to false when that failed. */
    apply: (spec: SourceSpec) => Promise<boolean>;
    /** The comparison is of two files, so the side must stay a file. */
    file: boolean;
  }
</script>

<script lang="ts">
  import { tick } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { api, sourceText, type SourceInfo } from "./api";
  import SidePicker, { toSpec, type PickerValue } from "./SidePicker.svelte";

  let {
    info,
    suffix = "",
    modified = false,
    edit,
    title = "",
  }: { info: SourceInfo; suffix?: string; modified?: boolean; edit?: SideEdit; title?: string } = $props();

  const badge = $derived(
    { folder: ["DIR", "fs"], archive: ["ARC", "arc"], gitRev: ["GIT", "git"], gitWorktree: ["WT", "wt"] }[info.kind],
  );

  let editing = $state(false);
  let text = $state("");
  let original: SourceSpec | null = null;
  let busy = $state(false);
  let input = $state<HTMLInputElement>();
  let picking = $state(false);
  let picker = $state<PickerValue>({ kind: "folder", path: "", rev: "", subpath: "" });

  async function current(): Promise<SourceSpec> {
    try {
      return await edit!.spec();
    } catch {
      return { type: "local", path: info.display };
    }
  }

  async function startEdit() {
    if (!edit || editing || busy) return;
    original = await current();
    text = sourceText(original);
    editing = true;
    await tick();
    input?.focus();
    input?.select();
  }

  async function apply(spec: SourceSpec): Promise<boolean> {
    busy = true;
    try {
      return await edit!.apply(spec);
    } finally {
      busy = false;
    }
  }

  async function commit() {
    const typed = text.trim();
    if (!typed || (original && typed === sourceText(original))) {
      editing = false;
      return;
    }
    let spec = await api.parseSource(typed);
    // A worktree side stays a worktree when only its folder is retyped.
    if (original?.type === "gitWorktree" && spec.type === "local") spec = { type: "gitWorktree", repo: spec.path };
    if (await apply(spec)) editing = false;
    else input?.focus();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      commit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      editing = false;
    }
  }

  /**
   * Where the file dialog starts: next to the current file or folder, so its
   * siblings are listed. Paths inside archives start at the archive.
   */
  function dialogStart(spec: SourceSpec, directory: boolean): string | undefined {
    const path = (spec.type === "local" ? spec.path.split("!")[0] : spec.repo).replace(/[\\/]+$/, "");
    if (!directory) return path || undefined;
    const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"));
    const parent = cut > 0 ? path.slice(0, cut) : path;
    // `C:` alone means the current folder on that drive.
    return (parent.endsWith(":") ? `${parent}\\` : parent) || undefined;
  }

  async function browse() {
    if (!edit || busy) return;
    const spec = await current();
    if (spec.type === "gitRev") {
      openPicker(spec);
      return;
    }
    const wantsFile = edit.file || info.kind === "archive";
    const picked = await open({
      directory: !wantsFile,
      multiple: false,
      defaultPath: dialogStart(spec, !wantsFile),
      title: wantsFile ? "Compare with another file" : "Compare with another folder",
    });
    if (typeof picked === "string") await apply({ type: "local", path: picked });
  }

  function pickerValue(spec: SourceSpec): PickerValue {
    switch (spec.type) {
      case "local":
        return { kind: edit?.file || info.kind === "archive" ? "file" : "folder", path: spec.path, rev: "", subpath: "" };
      case "gitRev":
        return { kind: "gitRev", path: spec.repo, rev: spec.rev, subpath: spec.subpath ?? "" };
      case "gitWorktree":
        return { kind: "gitWorktree", path: spec.repo, rev: "", subpath: "" };
    }
  }

  async function openPicker(spec?: SourceSpec) {
    if (!edit || busy) return;
    picker = pickerValue(spec ?? (await current()));
    editing = false;
    picking = true;
  }

  async function submitPicker(e: SubmitEvent) {
    e.preventDefault();
    if (!picker.path.trim()) return;
    if (await apply(toSpec(picker))) picking = false;
  }
</script>

<div class="bar" class:editable={!!edit} title={editing ? "" : info.commitSummary ?? info.display}>
  <span class="badge {badge[1]}">{badge[0]}</span>
  {#if editing}
    <input
      class="path mono edit"
      bind:this={input}
      bind:value={text}
      onkeydown={onKey}
      onblur={() => !busy && (editing = false)}
      disabled={busy}
      spellcheck="false"
      aria-label="{title} path"
    />
  {:else if edit}
    <button class="path mono" onclick={startEdit} title="Click to compare with another path ({info.display})">
      <span dir="ltr">{info.display}{suffix ? ` › ${suffix}` : ""}</span>
    </button>
  {:else}
    <span class="path mono"><span dir="ltr">{info.display}{suffix ? ` › ${suffix}` : ""}</span></span>
  {/if}
  {#if info.rev}
    <span class="ref mono">⎇ {info.rev}{info.commit && info.commit !== info.rev ? ` · ${info.commit}` : ""}</span>
  {/if}
  {#if modified}<span class="mod" title="Unsaved changes">● modified</span>{/if}
  {#if !info.writable}<span class="ro">read-only</span>{/if}
  {#if edit}
    <button
      class="icon"
      onclick={browse}
      disabled={busy}
      title={info.kind === "gitRev" ? "Choose another revision" : edit.file || info.kind === "archive" ? "Choose another file" : "Choose another folder"}
    >{info.kind === "gitRev" ? "⎇" : "📂"}</button>
    <button class="icon" onclick={() => openPicker()} disabled={busy} title="Change this side: folder, file, Git ref or worktree">▾</button>
  {/if}

  {#if picking}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <form
      class="popover"
      onsubmit={submitPicker}
      onkeydown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          picking = false;
        }
      }}
    >
      <SidePicker title="{title} side" bind:value={picker} />
      <footer>
        <button type="button" onclick={() => (picking = false)}>Cancel</button>
        <button type="submit" class="primary" disabled={busy || !picker.path.trim()}>Compare</button>
      </footer>
    </form>
  {/if}
</div>

<style>
  .bar {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 8px;
    background: var(--panel-2);
    min-width: 0;
  }
  .badge {
    font-size: 10.5px;
    font-weight: 600;
    padding: 0 6px;
    border-radius: 3px;
    color: #fff;
    background: #3d6e9e;
  }
  .badge.git {
    background: #b2502d;
  }
  .badge.arc {
    background: #6b5ab8;
  }
  .badge.wt {
    background: #3f7d4f;
  }
  .path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    background: var(--input);
    border: 1px solid var(--line);
    padding: 1px 6px;
    border-radius: 3px;
    /* Long paths lose their beginning, not the interesting end. */
    direction: rtl;
    text-align: left;
    font: inherit;
    color: inherit;
  }
  button.path {
    cursor: text;
  }
  button.path:hover {
    border-color: var(--accent);
  }
  .path.edit {
    direction: ltr;
    border-color: var(--accent);
    outline: none;
  }
  .icon {
    padding: 0 6px;
    line-height: 18px;
  }
  .ref {
    color: #e6a974;
    white-space: nowrap;
  }
  .ro {
    color: var(--dim);
    font-size: 11px;
  }
  .mod {
    color: var(--accent);
    font-size: 11px;
    white-space: nowrap;
  }
  .popover {
    position: absolute;
    top: calc(100% + 2px);
    left: 8px;
    right: 8px;
    min-width: 360px;
    z-index: 20;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 12px;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 6px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.35);
  }
  .popover footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
</style>
