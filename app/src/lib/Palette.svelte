<script lang="ts">
  // The command palette: every action of the app and the current tab, found by typing.
  import { onMount, tick } from "svelte";
  import { matchScore, type Command } from "./commands";

  let { commands, onClose }: { commands: Command[]; onClose: () => void } = $props();

  let query = $state("");
  let index = $state(0);
  let input = $state<HTMLInputElement>();
  let list = $state<HTMLDivElement>();

  const shown = $derived.by(() => {
    if (!query.trim()) return commands;
    return commands
      .map((c, order) => ({ c, order, score: matchScore(`${c.group}: ${c.title}`, query) }))
      .filter((m) => m.score !== null)
      // Best matches first; ones that apply now before dimmed ones; otherwise as listed.
      .sort((a, b) => a.score! - b.score! || Number(a.c.enabled === false) - Number(b.c.enabled === false) || a.order - b.order)
      .map((m) => m.c);
  });

  onMount(() => input?.focus());

  // Start on the first command that applies now.
  $effect(() => {
    index = Math.max(0, shown.findIndex((c) => c.enabled !== false));
  });

  async function move(step: number) {
    if (!shown.length) return;
    index = (index + step + shown.length) % shown.length;
    await tick();
    list?.querySelector(".on")?.scrollIntoView({ block: "nearest" });
  }

  function run(c: Command | undefined) {
    if (!c || c.enabled === false) return;
    onClose();
    c.run();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "ArrowDown") move(1);
    else if (e.key === "ArrowUp") move(-1);
    else if (e.key === "PageDown") move(8);
    else if (e.key === "PageUp") move(-8);
    else if (e.key === "Enter") run(shown[index]);
    else if (e.key === "Escape") onClose();
    else return;
    e.preventDefault();
    e.stopPropagation();
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
<div class="backdrop" onmousedown={(e) => e.target === e.currentTarget && onClose()}>
  <div class="palette" role="dialog" aria-label="Command palette">
    <input
      bind:this={input}
      bind:value={query}
      onkeydown={onKey}
      placeholder="Type a command…"
      spellcheck="false"
      aria-label="Command"
    />
    <div class="list" bind:this={list} role="listbox">
      {#each shown as c, i (i)}
        <div
          class="item"
          class:on={i === index}
          class:off={c.enabled === false}
          role="option"
          aria-selected={i === index}
          tabindex="-1"
          onmouseenter={() => (index = i)}
          onmousedown={(e) => {
            e.preventDefault();
            run(c);
          }}
        >
          <span class="group">{c.group}</span>
          <span class="title">{c.title}</span>
          {#if c.keys}<kbd>{c.keys}</kbd>{/if}
        </div>
      {:else}
        <p class="none">No matching command</p>
      {/each}
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 95;
    background: rgba(0, 0, 0, 0.25);
    display: flex;
    justify-content: center;
    align-items: flex-start;
    padding-top: 12vh;
  }
  .palette {
    width: min(640px, calc(100vw - 32px));
    max-height: 70vh;
    display: flex;
    flex-direction: column;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.45);
    overflow: hidden;
  }
  input {
    margin: 10px;
    padding: 7px 10px;
    font-size: 14px;
  }
  .list {
    overflow: auto;
    padding-bottom: 6px;
  }
  .item {
    display: flex;
    gap: 10px;
    align-items: baseline;
    padding: 5px 14px;
    cursor: default;
  }
  .item.on {
    background: var(--sel);
  }
  .item.off {
    opacity: 0.45;
  }
  .group {
    flex: none;
    min-width: 76px;
    font-size: 11px;
    color: var(--dim);
  }
  .title {
    flex: 1;
  }
  kbd {
    font: inherit;
    font-size: 11px;
    color: var(--dim);
  }
  .none {
    margin: 8px 14px;
    color: var(--dim);
  }
</style>
