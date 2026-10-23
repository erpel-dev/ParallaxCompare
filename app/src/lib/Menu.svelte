<script lang="ts" module>
  export type MenuItem =
    | { label: string; keys?: string; detail?: string; tone?: "primary" | "danger"; disabled?: boolean; run: () => void }
    | "separator";

  export interface MenuContext {
    side: "left" | "right";
    title: string;
    detail: string;
    badge: string;
  }
</script>

<script lang="ts">
  // A context menu at a point on the screen. Arrow keys move, Enter runs,
  // Esc or a click elsewhere closes.
  import { tick } from "svelte";

  let { items, x, y, context, returnFocus, onClose }: {
    items: MenuItem[];
    x: number;
    y: number;
    context?: MenuContext;
    returnFocus?: HTMLElement;
    onClose: () => void;
  } = $props();

  let el = $state<HTMLDivElement>();
  let pos = $state({ left: 0, top: 0 });
  let focus = $state(-1);
  const uid = $props.id();
  let reposition = 0;

  const runnable = $derived(
    items.map((item, i) => (item !== "separator" && !item.disabled ? i : -1)).filter((i) => i >= 0),
  );

  $effect(() => {
    // Right-clicking another entry can replace an already open menu.
    void items;
    const point = { x, y };
    if (!el) return;
    focus = -1;
    const token = ++reposition;
    tick().then(() => {
      if (!el || token !== reposition) return;
      const r = el.getBoundingClientRect();
      pos = {
        left: Math.max(4, Math.min(point.x, window.innerWidth - r.width - 4)),
        top: Math.max(4, Math.min(window.innerHeight - r.height - 4, point.y + r.height > window.innerHeight - 4 ? point.y - r.height : point.y)),
      };
      el.focus();
    });
  });

  function choose(item: MenuItem) {
    if (item === "separator" || item.disabled) return;
    close(true);
    item.run();
  }

  function close(restore = false) {
    onClose();
    if (restore) returnFocus?.focus({ preventScroll: true });
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") close(true);
    else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      const at = runnable.indexOf(focus);
      const step = e.key === "ArrowDown" ? 1 : -1;
      focus = at < 0 ? (step > 0 ? runnable[0] : runnable.at(-1)) ?? -1
        : runnable[(at + step + runnable.length) % runnable.length] ?? -1;
    } else if (e.key === "Home" || e.key === "End") {
      focus = (e.key === "Home" ? runnable[0] : runnable.at(-1)) ?? -1;
    } else if (e.key === "Tab") {
      close(true);
      return;
    } else if (e.key === "Enter" && focus >= 0) choose(items[focus]);
    else {
      const item = items.find((item) => item !== "separator" && item.keys && matchesShortcut(item.keys, e));
      if (!item) {
        // Let global commands run without leaving an old context menu behind.
        if (!["Control", "Shift", "Alt", "Meta"].includes(e.key) && (e.ctrlKey || e.metaKey || e.altKey || /^F\d+$/.test(e.key))) close();
        return;
      }
      choose(item);
    }
    e.preventDefault();
    e.stopPropagation();
    tick().then(() => el?.querySelector(`#${CSS.escape(uid)}-${focus}`)?.scrollIntoView({ block: "nearest" }));
  }

  function matchesShortcut(keys: string, e: KeyboardEvent): boolean {
    const parts = keys.split("+");
    if (parts.length === 1 && !/^F\d+$/.test(keys)) return false;
    const last = parts.at(-1)!;
    const key = ({ "←": "ArrowLeft", "→": "ArrowRight" } as Record<string, string>)[last] ?? last;
    return key.toLowerCase() === e.key.toLowerCase()
      && parts.includes("Ctrl") === (e.ctrlKey || e.metaKey)
      && parts.includes("Alt") === e.altKey
      && parts.includes("Shift") === e.shiftKey;
  }

  function outside(e: MouseEvent) {
    if (el && !el.contains(e.target as Node)) onClose();
  }
</script>

<svelte:window onmousedown={outside} onblur={onClose} onresize={onClose} />

<div
  class="menu"
  role="menu"
  aria-label={context ? `${context.side === "left" ? "Left" : "Right"} context menu` : "Actions"}
  aria-activedescendant={focus >= 0 ? `${uid}-${focus}` : undefined}
  data-side={context?.side}
  tabindex="-1"
  bind:this={el}
  style:left="{pos.left}px"
  style:top="{pos.top}px"
  onkeydown={onKey}
  oncontextmenu={(e) => e.preventDefault()}
>
  {#if context}
    <div class="context">
      <div class="origin"><strong>{context.side === "left" ? "LEFT" : "RIGHT"}</strong><span>{context.badge}</span></div>
      <div class="title" title={context.title}>{context.title}</div>
      <div class="detail" title={context.detail}>{context.detail}</div>
    </div>
  {/if}
  {#each items as item, i (i)}
    {#if item === "separator"}
      <hr />
    {:else}
      <button
        role="menuitem"
        id={`${uid}-${i}`}
        tabindex="-1"
        class:focus={i === focus}
        class:primary={item.tone === "primary"}
        class:danger={item.tone === "danger"}
        disabled={item.disabled}
        title={item.detail}
        onmouseenter={() => (focus = item.disabled ? -1 : i)}
        onclick={() => choose(item)}
      >
        <span><span>{item.label}</span>{#if item.detail}<small>{item.detail}</small>{/if}</span>
        {#if item.keys}<kbd>{item.keys}</kbd>{/if}
      </button>
    {/if}
  {/each}
</div>

<style>
  .menu {
    position: fixed;
    z-index: 90;
    min-width: 230px;
    max-width: min(420px, calc(100vw - 8px));
    max-height: calc(100dvh - 8px);
    overflow-y: auto;
    --menu-accent: var(--accent);
    padding: 4px 0;
    display: flex;
    flex-direction: column;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 6px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.35);
    outline: none;
  }
  .menu[data-side="right"] { --menu-accent: #b798ee; }
  @media (prefers-color-scheme: light) {
    .menu[data-side="right"] { --menu-accent: #7550c7; }
  }
  .context { padding: 9px 14px; margin-bottom: 4px; border-bottom: 1px solid var(--line); }
  .origin { display: flex; justify-content: space-between; align-items: center; gap: 24px; margin-bottom: 7px; color: var(--menu-accent); font-size: 11px; }
  .origin span { color: var(--dim); font-size: 10px; }
  .title { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .detail { margin-top: 4px; color: var(--dim); font-size: 11px; white-space: pre-line; overflow-wrap: anywhere; }
  small { display: block; font-size: 11px; color: var(--dim); margin-top: 3px; font-weight: normal; }
  button.primary { color: var(--menu-accent); background: color-mix(in srgb, var(--menu-accent) 10%, var(--panel)); font-weight: 600; }
  button.danger { color: var(--error); }
  button:disabled { color: var(--dim); opacity: .65; }
  button {
    display: flex;
    justify-content: space-between;
    gap: 24px;
    border: none;
    border-radius: 0;
    background: none;
    text-align: left;
    padding: 5px 14px;
  }
  button.focus {
    background: var(--sel);
  }
  kbd {
    font: inherit;
    font-size: 11px;
    color: var(--dim);
  }
  hr {
    width: 100%;
    border: none;
    border-top: 1px solid var(--line);
    margin: 4px 0;
  }
</style>
