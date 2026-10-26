<script lang="ts">
  // A path field with suggestions: recently compared paths, and the entries of the
  // folder typed so far (Tab completes, ↑/↓ choose, Enter takes the highlighted one).
  // Archives are marked; Tab on one enters it (`name.zip!/`) and lists its entries.
  import { tick } from "svelte";
  import { api, type Completion } from "./api";
  import { recentPaths } from "./recent";

  let {
    value = $bindable(),
    placeholder = "",
    disabled = false,
    label = "",
    input = $bindable(),
    onEnter,
    onEscape,
    onblur,
  }: {
    value: string;
    placeholder?: string;
    disabled?: boolean;
    label?: string;
    input?: HTMLInputElement;
    onEnter?: () => void;
    onEscape?: () => void;
    onblur?: () => void;
  } = $props();

  let open = $state(false);
  let completions = $state<Completion[]>([]);
  /** An archive is being read to list its entries (large compressed tars take a while). */
  let reading = $state("");
  let recent = $state<string[]>([]);
  let highlight = $state(-1);
  let list = $state<HTMLDivElement>();
  let request = 0;

  const isFolder = (p: string) => p.endsWith("/") || p.endsWith("\\");

  /** Recent paths that contain what is typed, then completions not already listed. */
  const items = $derived.by(() => {
    const typed = value.trim().toLowerCase();
    const recents = recent.filter((p) => p.toLowerCase().includes(typed) && p !== value).slice(0, 8);
    const rest = completions.filter((c) => !recents.includes(c.text) && c.text !== value);
    return [
      ...recents.map((p) => ({ path: p, recent: true, kind: undefined, detail: undefined })),
      ...rest.map((c) => ({ path: c.text, recent: false, kind: c.kind, detail: c.detail })),
    ];
  });

  /** The archive whose entries `text` asks for, when it names a place inside one. */
  const inArchive = (text: string) => /^(.*?\.[^/\\!]+)!/.exec(text)?.[1] ?? "";

  async function refresh() {
    const id = ++request;
    const text = value;
    // Reading a large archive takes a moment: say so, but only when it does.
    const archive = inArchive(text);
    const slow = archive ? setTimeout(() => id === request && (reading = archive.split(/[\\/]/).pop() ?? archive), 250) : undefined;
    const found = text.trim() ? await api.completePath(text).catch(() => []) : [];
    clearTimeout(slow);
    if (id === request) {
      completions = found;
      reading = "";
    }
  }

  /** Tab on an archive enters it: its entries are listed next. */
  async function enter(archive: string) {
    value = `${archive}!/`;
    highlight = -1;
    open = true;
    await toEnd();
    await refresh();
  }

  /** The caret at the end, scrolled into view: completing changes the end of the text. */
  async function toEnd() {
    await tick();
    if (!input) return;
    input.setSelectionRange(value.length, value.length);
    input.scrollLeft = input.scrollWidth;
  }

  /** The folder typed so far: suggestions inside it show only their own part. */
  const typedDir = $derived(value.slice(0, Math.max(value.lastIndexOf("/"), value.lastIndexOf("\\")) + 1));
  const shown = (path: string) => (typedDir.length > 1 && path.startsWith(typedDir) && path.length > typedDir.length ? path.slice(typedDir.length) : path);

  let timer: ReturnType<typeof setTimeout> | undefined;
  function typed() {
    open = true;
    highlight = -1;
    clearTimeout(timer);
    timer = setTimeout(refresh, 120);
  }

  // A field that already holds a path is selected as a whole when it gets the
  // focus, so typing replaces it; the click that focused it must not undo that.
  let keepSelection = false;
  function focused() {
    recent = recentPaths();
    if (value && input) {
      input.select();
      keepSelection = true;
    }
    // The list opens on typing or ↓, so it does not cover the fields below on a click.
    refresh();
  }

  function commonPrefix(paths: string[]): string {
    let prefix = paths[0] ?? "";
    for (const p of paths) {
      let i = 0;
      while (i < prefix.length && i < p.length && prefix[i].toLowerCase() === p[i].toLowerCase()) i++;
      prefix = prefix.slice(0, i);
    }
    return prefix;
  }

  async function choose(path: string, submit: boolean) {
    value = path;
    highlight = -1;
    completions = [];
    if (submit && !isFolder(path)) {
      open = false;
      onEnter?.();
    } else {
      await refresh();
      input?.focus();
      await toEnd();
    }
  }

  async function complete() {
    const picked = highlight >= 0 ? items[highlight] : undefined;
    // An archive on disk is entered with `!/`; one inside an archive is already a folder path.
    if (picked?.kind === "archive") return picked.path.endsWith("/") ? choose(picked.path, false) : enter(picked.path);
    await refresh();
    const exact = completions.find((c) => c.kind === "archive" && c.text === value);
    if (exact) return enter(exact.text);
    if (completions.length === 1) {
      const [only] = completions;
      return only.kind === "archive" && !only.text.endsWith("/") ? enter(only.text) : choose(only.text, false);
    }
    const prefix = commonPrefix(completions.map((c) => c.text));
    if (prefix.length > value.length) {
      value = prefix;
      await toEnd();
      await refresh();
    } else if (completions.length) {
      move(1);
    }
  }

  async function move(step: number) {
    if (!items.length) return;
    open = true;
    highlight = (highlight + step + items.length + 1) % (items.length + 1);
    if (highlight === items.length) highlight = -1;
    await tick();
    list?.querySelector(".on")?.scrollIntoView({ block: "nearest" });
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Tab" && !e.shiftKey && value.trim()) {
      e.preventDefault();
      complete();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      move(e.key === "ArrowDown" ? 1 : -1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (open && highlight >= 0 && items[highlight]) choose(items[highlight].path, true);
      else {
        open = false;
        onEnter?.();
      }
    } else if (e.key === "Escape") {
      if (open && items.length && highlight >= 0) {
        e.preventDefault();
        e.stopPropagation();
        highlight = -1;
        open = false;
      } else if (onEscape) {
        e.preventDefault();
        e.stopPropagation();
        open = false;
        onEscape();
      } else {
        open = false;
      }
    }
  }
</script>

<div class="pathinput">
  <input
    type="text"
    class="mono"
    bind:this={input}
    bind:value
    {placeholder}
    {disabled}
    aria-label={label || undefined}
    spellcheck="false"
    autocomplete="off"
    oninput={typed}
    onfocus={focused}
    onmouseup={(e) => {
      if (keepSelection) e.preventDefault();
      keepSelection = false;
    }}
    onkeydown={onKey}
    onblur={() => {
      open = false;
      onblur?.();
    }}
  />
  {#if open && items.length}
    <div class="suggest" bind:this={list} role="listbox">
      {#each items as item, i (item.path + item.recent)}
        <!-- Mousedown, not click: the input must not lose focus first. -->
        <div
          class="item mono"
          class:on={i === highlight}
          role="option"
          aria-selected={i === highlight}
          tabindex="-1"
          onmousedown={(e) => {
            e.preventDefault();
            choose(item.path, true);
          }}
        >
          <span class="icon">{item.recent ? "🕘" : item.kind === "archive" ? "🗜" : isFolder(item.path) ? "📁" : item.kind === "ref" || item.path.includes("@") ? "⎇" : "📄"}</span>
          <span class="text" title={item.path}>{#if !item.recent && shown(item.path) !== item.path}<span class="dirpart">…/</span>{shown(item.path)}{:else}{item.path}{/if}</span>
          {#if item.kind === "archive"}<span class="tag">ARC</span>{/if}
          {#if item.detail}<span class="detail">{item.detail}{#if item.kind === "archive" && i === highlight} · Tab opens{/if}</span>{/if}
        </div>
      {/each}
      {#if reading}<div class="item reading">Reading {reading}…</div>{/if}
    </div>
  {:else if open && reading}
    <div class="suggest"><div class="item reading">Reading {reading}…</div></div>
  {/if}
</div>

<style>
  .pathinput {
    position: relative;
    flex: 1;
    min-width: 0;
    display: flex;
  }
  input {
    flex: 1;
    min-width: 0;
  }
  .suggest {
    position: absolute;
    top: calc(100% + 2px);
    left: 0;
    right: 0;
    z-index: 40;
    max-height: 260px;
    overflow: auto;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 4px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.3);
  }
  .item {
    display: flex;
    gap: 6px;
    padding: 3px 8px;
    cursor: default;
    white-space: nowrap;
  }
  .item.on,
  .item:hover {
    background: var(--sel);
  }
  .text {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .dirpart {
    color: var(--dim);
  }
  .tag {
    flex: none;
    align-self: center;
    font-family: system-ui, sans-serif;
    font-size: 9.5px;
    font-weight: 600;
    padding: 0 4px;
    border-radius: 3px;
    color: #fff;
    background: #6b5ab8;
  }
  .detail {
    flex: none;
    margin-left: auto;
    padding-left: 12px;
    color: var(--dim);
    font-family: system-ui, sans-serif;
    font-size: 11px;
  }
  .reading {
    color: var(--dim);
    font-style: italic;
  }
  .icon {
    flex: none;
    width: 1.3em;
  }
  .text {
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
