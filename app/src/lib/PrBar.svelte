<script lang="ts">
  import { api } from "./api";
  import { prConversations, reviewSummary, linesApply, type PrState } from "./prConversation.svelte";
  import type { Snippet } from "svelte";

  let {
    session,
    pr,
    panelOpen = false,
    onPanel,
    onReopen,
    extra,
  }: {
    session: number;
    pr: PrState;
    panelOpen?: boolean;
    onPanel?: () => void;
    /** Opens the pull request again, at its current head. */
    onReopen?: () => void;
    extra?: Snippet;
  } = $props();

  const data = $derived(pr.data);
  const reviews = $derived(data ? reviewSummary(data) : []);
  const people = $derived(reviews.filter((r) => !r.bot && (r.state === "APPROVED" || r.state === "CHANGES_REQUESTED")));
  const bots = $derived(new Set([...(data?.threads ?? []).flatMap((t) => t.comments.filter((c) => c.bot).map((c) => c.author)), ...reviews.filter((r) => r.bot).map((r) => r.author)]).size);
  const open = $derived(data ? data.threads.filter((t) => !t.resolved).length : 0);
  const ci = $derived(data?.annotations.length ?? 0);
  const checks = $derived(data?.checks);
  const totalChecks = $derived(checks ? checks.passed + checks.failed + checks.pending : 0);
  const host = $derived.by(() => {
    try {
      return new URL(pr.info.url).host;
    } catch {
      return "github.com";
    }
  });
  const signIn = $derived(!!pr.error && /auth|login|401|credential|token/i.test(pr.error));
</script>

<div class="prbar" role="region" aria-label="Pull request">
  <b class="title" title={pr.info.title}>#{pr.info.number} {pr.info.title}</b>
  {#if data}
    <span class="dim">{data.author ? `by ${data.author}` : ""} · {data.state.toLowerCase()}</span>
    {#each people as r (r.author)}
      <span class="pill" class:ok={r.state === "APPROVED"} class:bad={r.state === "CHANGES_REQUESTED"} title="{r.author}: {r.state === 'APPROVED' ? 'approved' : 'requested changes'}">
        {r.state === "APPROVED" ? "✓" : "✕"} {r.author}
      </span>
    {/each}
    {#if bots}<span class="pill dim" title="Review bots that commented">{bots} {bots === 1 ? "bot" : "bots"}</span>{/if}
    {#if totalChecks}
      <span class="pill" class:ok={!checks!.failed && !checks!.pending} class:bad={checks!.failed > 0} title="{checks!.passed} passed, {checks!.failed} failed, {checks!.pending} running, on the compared head">
        Checks {checks!.passed} / {totalChecks}
      </span>
    {/if}
    {#if onPanel}
      <button class="small" class:on={panelOpen} onclick={onPanel} title="Show the threads, review findings, CI findings and the conversation of this pull request">
        💬 {open} {open === 1 ? "thread" : "threads"}{#if ci}{" "}· ▲ {ci} CI {ci === 1 ? "finding" : "findings"}{/if} ›
      </button>
    {/if}
    {@render extra?.()}
  {:else if pr.loading}
    <span class="dim">Reading comments and reviews…</span>
  {:else if pr.error}
    <span class="err" title={pr.error}>{signIn ? `Comments need gh signed in: gh auth login --hostname ${host}` : "Comments could not be read."}</span>
  {/if}
  <span class="grow"></span>
  {#if data && !linesApply(data)}
    <span class="moved" title="Comments placed on lines refer to the new head; they are listed, not placed, here.">The pull request has a newer head.</span>
    {#if onReopen}<button class="small" onclick={onReopen}>Open the new head</button>{/if}
  {/if}
  <button class="link" onclick={() => api.openPullRequestLink(pr.info.url, pr.info.url).catch(() => {})} title={pr.info.url}>Open on GitHub ↗</button>
  <button class="small" onclick={() => prConversations.load(session)} disabled={pr.loading} title={pr.at ? `Read ${new Date(pr.at).toLocaleTimeString()}; read again` : "Read again"} aria-label="Read the comments again">⟳</button>
</div>

<style>
  .prbar { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 10px; padding: 5px 10px; border-bottom: 1px solid var(--line); background: var(--panel-2); font-size: 12px; min-width: 0; }
  .title { font-weight: 600; max-width: 40ch; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .dim { color: var(--dim); }
  .err { color: var(--error); }
  .moved { color: var(--similar); }
  .grow { flex: 1; }
  .pill { display: inline-flex; align-items: center; gap: 4px; border: 1px solid var(--line); border-radius: 10px; padding: 0 8px; font-size: 11.5px; white-space: nowrap; }
  .pill.ok { color: #2d8a4e; border-color: currentColor; }
  .pill.bad { color: var(--diff); border-color: currentColor; }
  button.small { font-size: 12px; padding: 1px 8px; }
  button.on { background: var(--sel); border-color: var(--accent); }
  .link { border: none; background: none; color: var(--accent); padding: 0 4px; cursor: pointer; font-size: 12px; }
  @media (prefers-color-scheme: dark) { :global(:root:not([data-theme="light"])) .pill.ok { color: #6fcf8f; } }
  :global(:root[data-theme="dark"]) .pill.ok { color: #6fcf8f; }
</style>
