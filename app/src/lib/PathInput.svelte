<script lang="ts">
  // A path field with suggestions: recently compared paths, and the entries of the
  // folder typed so far (Tab completes, ↑/↓ choose, Enter takes the highlighted one).
  import { tick } from "svelte";
  import { api } from "./api";
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
  let completions = $state<string[]>([]);
  let recent = $state<string[]>([]);
  let highlight = $state(-1);
  let list = $state<HTMLDivElement>();
  let request = 0;

  const isFolder = (p: string) => p.endsWith("/") || p.endsWith("\\");

  /** Recent paths that contain what is typed, then completions not already listed. */
  const items = $derived.by(() => {
    const typed = value.trim().toLowerCase();
    const recents = recent.filter((p) => p.toLowerCase().includes(typed) && p !== value).slice(0, 8);
    const rest = completions.filter((c) => !recents.includes(c) && c !== value);
    return [...recents.map((p) => ({ path: p, recent: true })), ...rest.map((p) => ({ path: p, recent: false }))];
  });

  async function refresh() {
    const id = ++request;
    const text = value;
    const found = text.trim() ? await api.completePath(text).catch(() => []) : [];
    if (id === request) completions = found;
  }

  let timer: ReturnType<typeof setTimeout> | undefined;
  function typed() {
    open = true;
    highlight = -1;
    clearTimeout(timer);
    timer = setTimeout(refresh, 120);
  }

  function focused() {
    recent = recentPaths();
    open = true;
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
    }
  }

  async function complete() {
    await refresh();
    if (completions.length === 1) return choose(completions[0], false);
    const prefix = commonPrefix(completions);
    if (prefix.length > value.length) {
      value = prefix;
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
          <span class="icon">{item.recent ? "🕘" : isFolder(item.path) ? "📁" : item.path.includes("@") ? "⎇" : "📄"}</span>
          <span class="text">{item.path}</span>
        </div>
      {/each}
    </div>
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
  .icon {
    flex: none;
    width: 1.3em;
  }
  .text {
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
