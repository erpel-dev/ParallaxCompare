<script lang="ts" module>
  export type MenuItem =
    | { label: string; keys?: string; disabled?: boolean; run: () => void }
    | "separator";
</script>

<script lang="ts">
  // A context menu at a point on the screen. Arrow keys move, Enter runs,
  // Esc or a click elsewhere closes.
  import { onMount, tick } from "svelte";

  let { items, x, y, onClose }: { items: MenuItem[]; x: number; y: number; onClose: () => void } = $props();

  let el = $state<HTMLDivElement>();
  let pos = $state({ left: 0, top: 0 });
  let focus = $state(-1);

  const runnable = $derived(
    items.map((item, i) => (item !== "separator" && !item.disabled ? i : -1)).filter((i) => i >= 0),
  );

  onMount(async () => {
    pos = { left: x, top: y };
    await tick();
    // Keep the menu on screen.
    const r = el!.getBoundingClientRect();
    pos = {
      left: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)),
      top: Math.max(4, y + r.height > window.innerHeight - 4 ? y - r.height : y),
    };
    el!.focus();
  });

  function choose(item: MenuItem) {
    if (item === "separator" || item.disabled) return;
    onClose();
    item.run();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") onClose();
    else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      const at = runnable.indexOf(focus);
      const step = e.key === "ArrowDown" ? 1 : -1;
      focus = runnable[(at + step + runnable.length) % runnable.length] ?? -1;
    } else if (e.key === "Enter" && focus >= 0) choose(items[focus]);
    else return;
    e.preventDefault();
    e.stopPropagation();
  }

  function outside(e: MouseEvent) {
    if (el && !el.contains(e.target as Node)) onClose();
  }
</script>

<svelte:window onmousedown={outside} onblur={onClose} />

<div
  class="menu"
  role="menu"
  tabindex="-1"
  bind:this={el}
  style:left="{pos.left}px"
  style:top="{pos.top}px"
  onkeydown={onKey}
  oncontextmenu={(e) => e.preventDefault()}
>
  {#each items as item, i (i)}
    {#if item === "separator"}
      <hr />
    {:else}
      <button
        role="menuitem"
        class:focus={i === focus}
        disabled={item.disabled}
        onmouseenter={() => (focus = item.disabled ? -1 : i)}
        onclick={() => choose(item)}
      >
        <span>{item.label}</span>
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
    padding: 4px 0;
    display: flex;
    flex-direction: column;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 6px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.35);
    outline: none;
  }
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
