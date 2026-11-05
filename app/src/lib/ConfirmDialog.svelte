<script lang="ts">
  // The question of `ask` in ./confirm.svelte.ts, over the window.
  import { tick } from "svelte";
  import { answer, confirmation } from "./confirm.svelte";

  let ok = $state<HTMLButtonElement>();
  const q = $derived(confirmation.current);

  $effect(() => {
    if (q) tick().then(() => ok?.focus());
  });

  function onKey(e: KeyboardEvent) {
    if (!q) return;
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      answer(false);
    }
  }
</script>

<svelte:window onkeydown={onKey} />

{#if q}
  <div class="back" role="presentation">
    <div class="dialog {q.kind}" role="alertdialog" aria-modal="true" aria-labelledby="confirm-title" aria-describedby="confirm-message">
      {#if q.title}<h2 id="confirm-title">{q.title}</h2>{/if}
      <p id="confirm-message">{q.message}</p>
      <div class="buttons">
        <button onclick={() => answer(false)}>{q.cancelLabel}</button>
        <button class="primary" class:danger={q.kind === "warning"} bind:this={ok} onclick={() => answer(true)}>{q.okLabel}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .back {
    position: fixed;
    inset: 0;
    z-index: 200;
    display: grid;
    place-items: start center;
    padding-top: 12vh;
    background: rgb(0 0 0 / 0.3);
  }
  .dialog {
    width: min(480px, 90vw);
    display: grid;
    gap: 12px;
    padding: 18px 20px 16px;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    box-shadow: 0 20px 60px rgb(0 0 0 / 0.35);
  }
  .dialog.warning {
    border-top: 3px solid var(--similar);
  }
  h2 {
    margin: 0;
    font-size: 15px;
  }
  p {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: 50vh;
    overflow: auto;
    line-height: 1.45;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .primary.danger {
    background: var(--error);
    border-color: var(--error);
  }
</style>
