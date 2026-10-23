<script lang="ts" module>
  import type { SourceSpec } from "./api";

  /** Changing one side of a comparison from its path bar. */
  export interface SideEdit {
    /** The side as it is compared now (for a file opened from a folder comparison: that file). */
    spec: () => Promise<SourceSpec>;
    /** Compares against `spec` instead; resolves to false when that failed. */
    apply: (spec: SourceSpec) => Promise<boolean>;
    /** A PR URL supplies both sides and replaces the whole comparison. */
    openPullRequest: (url: string) => Promise<boolean>;
    /** The comparison is of two files, so the side must stay a file. */
    file: boolean;
  }
</script>

<script lang="ts">
  import { tick } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { api, isPullRequestUrl, sourceText, type FileHistory, type SourceInfo } from "./api";
  import type { Command } from "./commands";
  import { formatTime } from "./format";
  import PathInput from "./PathInput.svelte";
  import SidePicker from "./SidePicker.svelte";
  import { pickerFor, toSpec, type PickerValue, type PickerPreview, usable } from "./sourcePicker";
  import { notify } from "./toast.svelte";

  let {
    info,
    suffix = "",
    modified = false,
    onSave,
    edit,
    title = "",
  }: {
    info: SourceInfo;
    suffix?: string;
    modified?: boolean;
    /** Saves this side alone; shown while it has unsaved changes. */
    onSave?: () => void;
    edit?: SideEdit;
    title?: string;
  } = $props();

  const badge = $derived(
    ({
      // A file opened from a folder comparison is a file.
      folder: suffix ? ["FILE", "fs"] : ["DIR", "fs"],
      file: ["FILE", "fs"],
      archive: ["ARC", "arc"],
      gitRev: ["GIT", "git"],
      gitWorktree: ["WT", "wt"],
    })[info.kind],
  );

  let editing = $state(false);
  let text = $state("");
  let original: SourceSpec | null = null;
  let busy = $state(false);
  let input = $state<HTMLInputElement>();
  let picking = $state(false);
  let picker = $state<PickerValue>({ path: "" });
  let pickerPreview = $state<PickerPreview | null>(null);
  let pickerError = $state("");
  let menu = $state(false);
  let bar = $state<HTMLDivElement>();
  let menuError = $state("");
  /** The side as of opening the menu, to tell which actions apply. */
  let menuSpec = $state<SourceSpec | null>(null);
  let history = $state<FileHistory | null>(null);
  let historyOpen = $state(false);
  let historyError = $state("");
  let currentText = $state("");

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
    if (busy) return;
    const typed = text.trim();
    if (!typed || (original && typed === sourceText(original))) {
      editing = false;
      return;
    }
    busy = true;
    try {
      if (isPullRequestUrl(typed)) {
        if (await edit!.openPullRequest(typed)) editing = false;
        else input?.focus();
      } else {
        let spec = await api.parseSource(typed);
        // A worktree side stays a worktree when only its folder is retyped.
        if (original?.type === "gitWorktree" && spec.type === "local") spec = { type: "gitWorktree", repo: spec.path };
        if (await apply(spec)) editing = false;
        else input?.focus();
      }
    } catch (e) {
      notify(String(e));
    } finally {
      busy = false;
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

  async function openPicker(spec?: SourceSpec) {
    if (!edit || busy) return;
    picker = pickerFor(spec ?? (await current()));
    pickerError = "";
    pickerPreview = null;
    editing = false;
    menu = false;
    picking = true;
  }

  async function openMenu() {
    if (!edit || busy) return;
    if (menu) {
      menu = false;
      return;
    }
    menuError = "";
    menuSpec = await current();
    picking = historyOpen = false;
    menu = true;
  }

  const onDisk = (spec: SourceSpec | null) =>
    !!spec && (spec.type === "gitWorktree" || (spec.type === "local" && !spec.path.includes("!")));

  async function openWith(reveal: boolean) {
    try {
      await api.openSource(menuSpec!, reveal);
      menu = false;
    } catch (e) {
      menuError = String(e);
    }
  }

  async function copyPath() {
    try {
      await navigator.clipboard.writeText(sourceText(menuSpec!));
      menu = false;
      notify("Path copied", "info");
    } catch (e) {
      menuError = String(e);
    }
  }

  async function showHistory() {
    const spec = menuSpec ?? (await current());
    menu = false;
    history = null;
    historyError = "";
    currentText = sourceText(spec);
    historyOpen = true;
    try {
      history = await api.fileHistory(spec);
    } catch (e) {
      historyError = String(e);
    }
  }

  async function pickVersion(spec: SourceSpec) {
    if (await apply(spec)) historyOpen = false;
  }

  /** Closes the menu and popovers on a click elsewhere. */
  function outside(e: MouseEvent) {
    if ((menu || historyOpen) && bar && !bar.contains(e.target as Node)) menu = historyOpen = false;
  }

  /** Runs an action on the side as compared now; failures become a notice. */
  async function act(action: (spec: SourceSpec) => Promise<unknown>) {
    try {
      await action(await current());
    } catch (e) {
      notify(String(e));
    }
  }

  /** What this side offers in the command palette. */
  export function commands(): Command[] {
    if (!edit) return [];
    const group = `${title} side`;
    const list: Command[] = [
      { group, title: "Edit path…", run: startEdit },
      { group, title: info.kind === "gitRev" ? "Choose another revision…" : "Choose with the file dialog…", run: browse },
      { group, title: "Change source…", run: () => openPicker() },
    ];
    if (edit.file) {
      list.push({
        group,
        title: "Git history of this file…",
        run: () => {
          menuSpec = null;
          showHistory();
        },
      });
    }
    list.push(
      {
        group,
        title: "Open in default app",
        enabled: info.kind !== "gitRev" && info.kind !== "archive",
        run: () => act((s) => api.openSource(s, false)),
      },
      { group, title: "Show in file manager", enabled: info.kind !== "gitRev", run: () => act((s) => api.openSource(s, true)) },
      {
        group,
        title: "Copy path",
        run: () =>
          act(async (s) => {
            await navigator.clipboard.writeText(sourceText(s));
            notify("Path copied", "info");
          }),
      },
    );
    return list;
  }

  async function submitPicker(e: SubmitEvent) {
    e.preventDefault();
    if (!usable(pickerPreview, picker) && !isPullRequestUrl(picker.path)) return;
    pickerError = "";
    try {
      if (isPullRequestUrl(picker.path)) { if (await edit!.openPullRequest(picker.path)) picking = false; }
      else if (await apply(await toSpec(picker))) picking = false;
    } catch (e) { pickerError = String(e); }
  }
</script>

<svelte:window onmousedown={outside} />

<div class="bar" bind:this={bar} class:editable={!!edit} title={editing ? "" : info.commitSummary ?? info.display}>
  <span class="badge {badge[1]}">{badge[0]}</span>
  {#if editing}
    <div class="path edit">
      <PathInput
        bind:value={text}
        bind:input
        disabled={busy}
        label="{title} path"
        onEnter={commit}
        onEscape={() => (editing = false)}
        onblur={() => !busy && (editing = false)}
      />
    </div>
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
  {#if modified}
    <span class="mod" title="Unsaved changes">● modified</span>
    {#if onSave}
      <button class="save" onclick={onSave} title="Save the {title.toLowerCase()} side (Ctrl+S saves both)" aria-label="Save the {title.toLowerCase()} side">
        <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
          <path d="M2.5 1.5h9l3 3v9a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1v-11a1 1 0 0 1 1-1z" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" />
          <path d="M4.5 1.5v4h6v-4" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" />
          <rect x="4.5" y="9" width="7" height="5.5" rx="0.5" fill="none" stroke="currentColor" stroke-width="1.3" />
        </svg>
      </button>
    {/if}
  {/if}
  {#if !info.writable}<span class="ro">read-only</span>{/if}
  {#if edit}
    <button
      class="icon"
      onclick={browse}
      disabled={busy}
      title={info.kind === "gitRev" ? "Choose another revision" : edit.file || info.kind === "archive" ? "Choose another file" : "Choose another folder"}
    >{info.kind === "gitRev" ? "⎇" : "📂"}</button>
    <button class="icon" class:on={menu} onclick={openMenu} disabled={busy} title="More: change this side, history, open">▾</button>
  {/if}

  {#if menu}
    <div class="menu" role="menu">
      <button role="menuitem" onclick={() => openPicker(menuSpec ?? undefined)}>Change side…</button>
      {#if edit?.file}
        <button role="menuitem" onclick={showHistory}>Git history of this file…</button>
      {/if}
      <hr />
      <button role="menuitem" onclick={() => openWith(false)} disabled={!onDisk(menuSpec)}>Open in default app</button>
      <button role="menuitem" onclick={() => openWith(true)} disabled={menuSpec?.type === "gitRev"}>Show in file manager</button>
      <button role="menuitem" onclick={copyPath}>Copy path</button>
      {#if menuError}<p class="err">{menuError}</p>{/if}
    </div>
  {/if}

  {#if historyOpen}
    <div class="popover history">
      <header>Git history · {title} side</header>
      {#if historyError}
        <p class="err">{historyError}</p>
      {:else if !history}
        <p class="dim">Reading history…</p>
      {:else}
        <div class="commits">
          {#if history.worktree}
            <button class="commit" class:on={sourceText(history.worktree) === currentText} onclick={() => pickVersion(history!.worktree!)}>
              <span class="id mono">working tree</span><span class="summary">the file on disk, with uncommitted changes</span>
            </button>
          {/if}
          {#each history.commits as c (c.id)}
            <button class="commit" class:on={sourceText(c.spec) === currentText} onclick={() => pickVersion(c.spec)}>
              <span class="id mono">{c.shortId}</span>
              <span class="summary">{c.summary}</span>
              <span class="when">{formatTime(c.time)}</span>
            </button>
          {/each}
          {#if !history.commits.length}<p class="dim">No commit changed this file.</p>{/if}
        </div>
      {/if}
      <footer><button onclick={() => (historyOpen = false)}>Close</button></footer>
    </div>
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
      <SidePicker title="{title} side" bind:value={picker} bind:preview={pickerPreview} onEnter={() => bar?.querySelector<HTMLFormElement>("form.popover")?.requestSubmit()} />
      {#if pickerError}<p class="err" role="alert">{pickerError}</p>{/if}
      <footer>
        <button type="button" onclick={() => (picking = false)}>Cancel</button>
        <button type="submit" class="primary" disabled={busy || (!usable(pickerPreview, picker) && !isPullRequestUrl(picker.path))}>Compare</button>
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
    display: flex;
    padding: 0;
    border: none;
    overflow: visible;
  }
  .path.edit :global(input) {
    padding: 1px 6px;
    border-color: var(--accent);
  }
  .icon.on {
    border-color: var(--accent);
  }
  .menu {
    position: absolute;
    top: calc(100% + 2px);
    right: 8px;
    z-index: 25;
    display: flex;
    flex-direction: column;
    min-width: 220px;
    padding: 4px 0;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 6px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.35);
  }
  .menu button {
    border: none;
    border-radius: 0;
    background: none;
    text-align: left;
    padding: 5px 14px;
  }
  .menu button:hover:not(:disabled) {
    background: var(--sel);
  }
  .menu hr {
    width: 100%;
    border: none;
    border-top: 1px solid var(--line);
    margin: 4px 0;
  }
  .err {
    margin: 4px 14px;
    color: var(--error);
    font-size: 12px;
    white-space: normal;
  }
  .dim {
    color: var(--dim);
    margin: 6px 0;
  }
  .history header {
    font-weight: 600;
  }
  .commits {
    display: flex;
    flex-direction: column;
    max-height: 320px;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: 4px;
    background: var(--input);
  }
  .commit {
    display: flex;
    gap: 10px;
    border: none;
    border-radius: 0;
    background: none;
    padding: 4px 8px;
    text-align: left;
  }
  .commit:hover {
    background: var(--panel);
  }
  .commit.on {
    background: var(--sel);
  }
  .commit .id {
    flex: none;
    color: #e6a974;
  }
  .commit .summary {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .commit .when {
    flex: none;
    color: var(--dim);
    font-size: 11px;
  }
  .save {
    display: inline-flex;
    align-items: center;
    padding: 2px 6px;
    color: var(--accent);
    border-color: var(--accent);
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
