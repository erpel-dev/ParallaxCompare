<script lang="ts" module>
  /** Moving between comparisons in one tab; `null` when there is nowhere to go. */
  export interface NavActions {
    back: (() => void) | null;
    backTitle: string;
    forward: (() => void) | null;
    forwardTitle: string;
    up: (() => void) | null;
    upTitle: string;
  }
</script>

<script lang="ts">
  let { nav }: { nav: NavActions } = $props();
</script>

<span class="nav">
  <button onclick={() => nav.back?.()} disabled={!nav.back} title={nav.backTitle} aria-label="Back">←</button>
  <button onclick={() => nav.forward?.()} disabled={!nav.forward} title={nav.forwardTitle} aria-label="Forward">→</button>
  <button onclick={() => nav.up?.()} disabled={!nav.up} title={nav.upTitle} aria-label="Up">↑</button>
</span>

<style>
  .nav {
    display: inline-flex;
  }
  .nav button {
    padding-left: 6px;
    padding-right: 6px;
    border-radius: 0;
    margin-right: -1px;
  }
  .nav button:first-child {
    border-radius: 4px 0 0 4px;
  }
  .nav button:last-child {
    border-radius: 0 4px 4px 0;
    margin-right: 0;
  }
</style>
