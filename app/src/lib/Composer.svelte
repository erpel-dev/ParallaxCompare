<script lang="ts" module>
  /** A button of the composer; `review` marks the one Ctrl+Enter runs (adding to the review). */
  export interface ComposerAction {
    label: string;
    title?: string;
    primary?: boolean;
    review?: boolean;
    run: (text: string) => Promise<boolean> | boolean;
  }
</script>

<script lang="ts">
  import { tick } from "svelte";
  import { commentHtml } from "./markdown";
  import { unsent } from "./prConversation.svelte";

  let {
    heading,
    url,
    target,
    initial = "",
    suggestion = null,
    actions,
    busy = false,
    rows = 4,
    autofocus = true,
    onCancel,
  }: {
    heading: string;
    /** The pull request, and what is written about: where unsent text is kept. */
    url: string;
    target: string;
    initial?: string;
    /** The selected lines of the head, for "Suggest change"; null where GitHub cannot apply one. */
    suggestion?: string[] | null;
    actions: ComposerAction[];
    busy?: boolean;
    rows?: number;
    autofocus?: boolean;
    onCancel: () => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  let text = $state(unsent.get(url, target) || initial);
  let preview = $state(false);
  let area = $state<HTMLTextAreaElement>();
  const empty = $derived(!text.trim());

  $effect(() => {
    if (autofocus) tick().then(() => area?.focus());
  });
  $effect(() => unsent.set(url, target, text));

  async function run(action: ComposerAction) {
    if (empty || busy) return;
    if (await action.run(text)) {
      unsent.set(url, target, "");
      text = "";
    }
  }

  function key(e: KeyboardEvent) {
    // The composer's keys stay in it: none of the comparison's shortcuts apply here.
    e.stopPropagation();
    if (e.key === "Escape") {
      e.preventDefault();
      cancel();
    } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && !e.shiftKey) {
      e.preventDefault();
      const action = actions.find((a) => a.review) ?? actions.find((a) => a.primary);
      if (action) void run(action);
    }
  }

  function cancel() {
    unsent.set(url, target, "");
    onCancel();
  }

  /** Puts the selected lines into a suggestion block at the cursor, to edit there. */
  function suggest() {
    if (!suggestion || !area) return;
    const block = "```suggestion\n" + suggestion.join("\n") + "\n```\n";
    const at = area.selectionStart ?? text.length;
    const before = text.slice(0, at);
    text = before + (before && !before.endsWith("\n") ? "\n" : "") + block + text.slice(at);
    preview = false;
    tick().then(() => area?.focus());
  }

  function wrap(mark: string) {
    if (!area) return;
    const [a, b] = [area.selectionStart, area.selectionEnd];
    text = text.slice(0, a) + mark + text.slice(a, b) + mark + text.slice(b);
    tick().then(() => {
      area?.focus();
      area?.setSelectionRange(a + mark.length, b + mark.length);
    });
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="composer" onkeydown={key} onclick={(e) => e.stopPropagation()}>
  <div class="ch">
    <span class="h">{heading}</span>
    <span class="tabs">
      <button class:on={!preview} onclick={() => (preview = false)}>Write</button>
      <button class:on={preview} onclick={() => (preview = true)}>Preview</button>
    </span>
  </div>
  {#if preview}
    <!-- Rendered from escaped text, as comments are. -->
    <div class="pv">{#if empty}<span class="dim">Nothing to preview.</span>{:else}{@html commentHtml(text)}{/if}</div>
  {:else}
    <textarea bind:this={area} bind:value={text} {rows} placeholder="Markdown; shown to everyone once posted" spellcheck="true" aria-label={heading}></textarea>
  {/if}
  <div class="tools">
    {#if suggestion}<button class="sm" onclick={suggest} title="Puts the selected lines into a suggestion block, to change there; the author can apply it on GitHub">± Suggest change</button>{/if}
    <button class="sm" onclick={() => wrap("**")} title="Bold" aria-label="Bold"><b>B</b></button>
    <button class="sm" onclick={() => wrap("`")} title="Code" aria-label="Code">`c`</button>
    <span class="grow"></span>
    <button class="sm" onclick={cancel} disabled={busy}>Cancel</button>
    {#each actions as a (a.label)}
      <button class="sm" class:primary={a.primary} onclick={() => run(a)} disabled={empty || busy} title={a.title}>
        {a.label}{#if a.review}<span class="k">Ctrl+Enter</span>{/if}
      </button>
    {/each}
  </div>
</div>

<style>
  .composer { border: 1px solid var(--accent); border-radius: 6px; background: var(--bg); display: grid; min-width: 0; cursor: default; }
  .ch { display: flex; align-items: center; gap: 8px; padding: 3px 6px 3px 8px; border-bottom: 1px solid var(--line); font-size: 11.5px; }
  .h { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--dim); }
  .tabs { display: inline-flex; }
  .tabs button { font-size: 11px; padding: 0 7px; border-radius: 0; margin-left: -1px; }
  .tabs button:first-child { border-radius: 4px 0 0 4px; margin-left: 0; }
  .tabs button:last-child { border-radius: 0 4px 4px 0; }
  .tabs button.on { background: var(--sel); border-color: var(--accent); position: relative; }
  textarea { border: none; outline: none; resize: vertical; min-height: 60px; padding: 6px 8px; background: var(--bg); color: var(--text); font-family: var(--mono); font-size: 11.5px; line-height: 1.5; }
  .pv { padding: 6px 8px; min-height: 60px; line-height: 1.45; overflow-wrap: anywhere; font-size: 12px; }
  .pv :global(p) { margin: 0 0 6px; }
  .pv :global(code) { font-family: var(--mono); font-size: 11px; background: var(--panel); padding: 0 3px; border-radius: 3px; }
  .pv :global(pre) { margin: 4px 0; padding: 6px; background: var(--panel); border-radius: 4px; overflow-x: auto; }
  .pv :global(.sugg) { font-family: var(--mono); font-size: 11px; border: 1px solid var(--line); border-radius: 4px; overflow: hidden; margin: 4px 0; }
  .pv :global(.sugg div) { padding: 1px 6px; white-space: pre-wrap; }
  .pv :global(.sugg .h) { background: var(--panel); color: var(--dim); font-family: var(--ui); }
  .pv :global(.sugg .p) { background: var(--row-added); }
  .pv :global(.copy) { display: none; }
  .dim { color: var(--dim); }
  .tools { display: flex; flex-wrap: wrap; gap: 4px; align-items: center; padding: 4px 6px; border-top: 1px solid var(--line); }
  .grow { flex: 1; }
  .sm { font-size: 11.5px; padding: 1px 7px; }
  .primary { background: var(--accent); border-color: var(--accent); color: #fff; }
  .primary:disabled { opacity: 0.55; }
  .k { opacity: 0.75; font-size: 10.5px; margin-left: 6px; }
</style>
