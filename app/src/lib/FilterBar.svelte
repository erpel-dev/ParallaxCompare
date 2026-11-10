<script module lang="ts">
  export interface CountFilter {
    id: string;
    label: string;
    count: number;
    title?: string;
    tone?: "same" | "similar" | "different" | "left" | "right" | "error";
  }
</script>

<script lang="ts">
  import type { Snippet } from "svelte";

  let { items, value, onSelect, disabled = false, label = "Show", hint = "", legend, extra, compact = false, inline = false }: {
    items: CountFilter[];
    value: string;
    onSelect: (id: string) => void;
    disabled?: boolean;
    label?: string;
    hint?: string;
    /** One menu naming the active filter instead of a button per filter, for narrow windows. */
    compact?: boolean;
    /** Inside a toolbar: no bar of its own. */
    inline?: boolean;
    /** Controls that belong to the filters, at the end of the bar. */
    extra?: Snippet;
    /** What the statuses mean, opened from the "?" button. */
    legend?: Snippet;
  } = $props();

  let open = $state(false);
  let el = $state<HTMLDivElement>();

  /** Opens the legend; also from the command palette. */
  export function showLegend() {
    open = true;
  }
</script>

<svelte:window
  onmousedown={(e) => open && el && !el.contains(e.target as Node) && (open = false)}
  onkeydown={(e) => open && e.key === "Escape" && (open = false)}
/>

<div class="filters" class:inline role="group" aria-label="Filter comparison by status">
  <span class="label">{label}</span>
  {#if compact}
    <select class="pick" {disabled} value={value} aria-label="Show" title="Which entries to show"
      onchange={(e) => onSelect(e.currentTarget.value)}>
      {#each items as item (item.id)}<option value={item.id}>{item.label} ({item.count.toLocaleString()})</option>{/each}
      {#if !items.some((i) => i.id === value)}<option value={value}>Other filter</option>{/if}
    </select>
  {:else}
  {#each items as item (item.id)}
    <button class:on={value === item.id} aria-pressed={value === item.id} {disabled}
      data-filter={item.id} title={item.title ?? `Show ${item.label.toLowerCase()}`}
      onclick={() => onSelect(item.id)}>
      {#if item.tone}<span class="dot {item.tone}" aria-hidden="true"></span>{/if}
      {item.label} <b>{item.count.toLocaleString()}</b>
    </button>
  {/each}
  {/if}
  {#if legend}
    <div class="legendwrap" bind:this={el}>
      <button class="help" class:on={open} aria-expanded={open} title="What the statuses mean" aria-label="What the statuses mean" onclick={() => (open = !open)}>?</button>
      {#if open}<div class="legend" role="dialog" aria-label="What the statuses mean">{@render legend()}</div>{/if}
    </div>
  {/if}
  {#if extra}<span class="extra">{@render extra()}</span>{/if}
  {#if hint}<span class="hint">{hint}</span>{/if}
</div>

<style>
  .filters { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; padding: 7px 10px; border-bottom: 1px solid var(--line); background: var(--panel-2); }
  .filters.inline { padding: 0; border: none; background: none; flex-wrap: nowrap; }
  .filters.inline .label { display: none; }
  .pick { font-size: 12px; padding: 2px 4px; max-width: 190px; }
  .label { color: var(--dim); font-size: 10px; text-transform: uppercase; letter-spacing: 1px; margin-right: 4px; }
  button { display: inline-flex; align-items: center; gap: 6px; font-size: 12px; padding: 4px 8px; }
  button:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  b { background: var(--panel); padding: 0 4px; border-radius: 3px; font-size: 11px; font-weight: 600; font-variant-numeric: tabular-nums; }
  button.on b { background: var(--sel); }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--dim); flex-shrink: 0; }
  .similar { background: var(--similar); }
  .different { background: var(--diff); }
  .left { background: var(--left-only); }
  .right { background: var(--right-only); }
  .error { background: var(--error); }
  .hint { margin-left: auto; font-size: 11px; color: var(--dim); }
  .extra { margin-left: auto; display: inline-flex; align-items: center; gap: 6px; font-size: 12px; color: var(--dim); }
  .extra + .hint { margin-left: 0; }
  .legendwrap { position: relative; }
  .help { width: 22px; height: 22px; justify-content: center; padding: 0; border-radius: 50%; font-weight: 600; }
  .help.on { border-color: var(--accent); }
  .legend {
    position: absolute; top: calc(100% + 6px); left: 0; z-index: 70; width: 420px; max-width: 80vw;
    display: grid; gap: 7px; padding: 12px 14px; font-size: 12.5px; line-height: 1.4; white-space: normal;
    background: var(--panel); border: 1px solid var(--line); border-radius: 6px; box-shadow: 0 12px 40px rgba(0, 0, 0, 0.35);
  }
  .legend :global(.lrow) { display: grid; grid-template-columns: 18px 82px 1fr; gap: 6px; align-items: baseline; }
  .legend :global(.g) { font-family: var(--mono); text-align: center; }
  .legend :global(.foot) { border-top: 1px solid var(--line); padding-top: 8px; color: var(--dim); }
  .legend :global(.on) { color: var(--fg); font-weight: 600; }
  .legend :global(.g.sim) { color: var(--similar); }
  .legend :global(.g.diff) { color: var(--diff); }
  .legend :global(.g.left) { color: var(--left-only); }
  .legend :global(.g.right) { color: var(--right-only); }
</style>
