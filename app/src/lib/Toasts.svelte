<script lang="ts">
  import { dismiss, hold, toasts } from "./toast.svelte";
</script>

<div class="toasts" aria-live="polite">
  {#each toasts as t (t.id)}
    <div
      class="toast {t.kind}"
      role={t.kind === "error" ? "alert" : "status"}
      onmouseenter={() => hold(t.id, true)}
      onmouseleave={() => hold(t.id, false)}
    >
      <span class="icon">{t.kind === "error" ? "⚠" : "✓"}</span>
      <span class="text">{t.text}</span>
      <button onclick={() => dismiss(t.id)} title="Dismiss" aria-label="Dismiss">✕</button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: 14px;
    bottom: 38px;
    z-index: 100;
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-width: min(520px, calc(100vw - 28px));
    pointer-events: none;
  }
  .toast {
    pointer-events: auto;
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 9px 10px 9px 12px;
    background: var(--panel);
    border: 1px solid var(--line);
    border-left: 4px solid var(--accent);
    border-radius: 6px;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.3);
  }
  .toast.error {
    border-left-color: var(--error);
  }
  .icon {
    flex: none;
  }
  .error .icon {
    color: var(--error);
  }
  .text {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
  button {
    flex: none;
    border: none;
    background: none;
    color: var(--dim);
    padding: 0 4px;
  }
</style>
