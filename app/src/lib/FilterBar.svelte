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
  let { items, value, onSelect, disabled = false, label = "Show", hint = "" }: {
    items: CountFilter[];
    value: string;
    onSelect: (id: string) => void;
    disabled?: boolean;
    label?: string;
    hint?: string;
  } = $props();
</script>

<div class="filters" role="group" aria-label="Filter comparison by status">
  <span class="label">{label}</span>
  {#each items as item (item.id)}
    <button class:on={value === item.id} aria-pressed={value === item.id} {disabled}
      data-filter={item.id} title={item.title ?? `Show ${item.label.toLowerCase()}`}
      onclick={() => onSelect(item.id)}>
      {#if item.tone}<span class="dot {item.tone}" aria-hidden="true"></span>{/if}
      {item.label} <b>{item.count.toLocaleString()}</b>
    </button>
  {/each}
  {#if hint}<span class="hint">{hint}</span>{/if}
</div>

<style>
  .filters { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; padding: 7px 10px; border-bottom: 1px solid var(--line); background: var(--panel-2); }
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
</style>
