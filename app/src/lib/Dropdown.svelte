<script lang="ts">
  // A toolbar button that opens a panel of settings below it.
  import type { Snippet } from "svelte";

  let {
    label,
    title = "",
    changed = false,
    children,
  }: { label: string; title?: string; changed?: boolean; children: Snippet } = $props();

  let open = $state(false);
  let el = $state<HTMLSpanElement>();

  function outside(e: MouseEvent) {
    if (open && el && !el.contains(e.target as Node)) open = false;
  }
</script>

<svelte:window onmousedown={outside} />

<span class="dropdown" bind:this={el}>
  <button class:on={open} onclick={() => (open = !open)} {title} aria-expanded={open}>
    {label}{#if changed}<span class="dot" title="Not the default settings">●</span>{/if} ▾
  </button>
  {#if open}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="panel"
      onkeydown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          open = false;
        }
      }}
    >
      {@render children()}
    </div>
  {/if}
</span>

<style>
  .dropdown {
    position: relative;
  }
  .dot {
    color: var(--accent);
    font-size: 9px;
    margin-left: 4px;
    vertical-align: 2px;
  }
  .panel {
    position: absolute;
    top: calc(100% + 4px);
    right: 0;
    z-index: 60;
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-width: 260px;
    padding: 12px;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 6px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.35);
  }
  .panel :global(label) {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .panel :global(.field) {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .panel :global(.field > span) {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--dim);
  }
</style>
