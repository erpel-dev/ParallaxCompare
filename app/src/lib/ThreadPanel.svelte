<script lang="ts">
  import { api, type PrAnnotation, type PrConversation, type PrNote, type PrThread } from "./api";
  import { renderMarkdown, escapeHtml } from "./markdown";
  import { ago, linesApply } from "./prConversation.svelte";
  import { notify } from "./toast.svelte";

  let {
    data,
    path = null,
    scope = null,
    current = null,
    onGo,
    onAnnotation,
    onClearScope,
    onClose,
  }: {
    data: PrConversation;
    /** One file's threads; null for all files and the conversation. */
    path?: string | null;
    /** In the folder view: only what is in this file or folder (null: everything). */
    scope?: string | null;
    /** The thread (index in `data.threads`) at the selected row. */
    current?: number | null;
    /** Goes to a thread: selects its row, or opens its file. */
    onGo: (thread: number) => void;
    /** Goes to a CI finding: its line in this file, or its file. */
    onAnnotation?: (annotation: PrAnnotation) => void;
    onClearScope?: () => void;
    onClose: () => void;
  } = $props();

  type Tab = "open" | "resolved" | "outdated" | "conversation";
  let tab = $state<Tab>("open");
  // A file or folder of its own has no conversation tab.
  $effect(() => {
    if (scope && tab === "conversation") tab = "open";
  });
  let el = $state<HTMLElement>();

  const within = (p: string) => scope === null || scope === "" || p === scope || p.startsWith(scope + "/");
  const inFile = (t: PrThread) => (path === null ? within(t.path) : t.path === path);
  const placed = (t: PrThread) => linesApply(data) && !t.outdated && !t.fileLevel && t.line !== null;
  const threads = $derived(data.threads.map((thread, index) => ({ thread, index })).filter(({ thread }) => inFile(thread)));
  const lists = $derived({
    open: threads.filter(({ thread }) => !thread.resolved && !thread.outdated),
    resolved: threads.filter(({ thread }) => thread.resolved),
    outdated: threads.filter(({ thread }) => thread.outdated && !thread.resolved),
  });
  const annotations = $derived(data.annotations.filter((a) => (path === null ? within(a.path) : a.path === path)));
  const sorted = (list: { thread: PrThread; index: number }[]) =>
    [...list].sort((a, b) => a.thread.path.localeCompare(b.thread.path) || (a.thread.line ?? a.thread.originalLine ?? 0) - (b.thread.line ?? b.thread.originalLine ?? 0));

  // The selected row's thread comes into view in the panel.
  $effect(() => {
    if (current === null || !el) return;
    const t = data.threads[current];
    if (t && !t.resolved && !t.outdated) tab = "open";
    queueMicrotask(() => el?.querySelector(`[data-thread="${current}"]`)?.scrollIntoView({ block: "nearest" }));
  });

  function where(t: PrThread): string {
    const file = path === null ? `${t.path} · ` : "";
    if (t.fileLevel) return `${file}on the file`;
    if (!placed(t)) return `${file}${t.outdated ? "was " : ""}line ${t.line ?? t.originalLine ?? "?"}${t.outdated ? "" : " on the newer head"}`;
    const lines = t.startLine && t.startLine !== t.line ? `${t.startLine}–${t.line}` : `${t.line}`;
    return `${file}${t.side} ${lines}`;
  }

  /** A comment as safe HTML, with its suggested changes shown as added lines. */
  function body(note: PrNote): string {
    const r = renderMarkdown(note.body);
    return r.html.replace(/<div class="md-suggestion" data-index="(\d+)"><\/div>/g, (_m, i) => {
      const lines = r.suggestions[Number(i)].lines;
      return `<div class="sugg"><div class="h">Suggested change <button class="copy" data-copy="${escapeHtml(lines.join("\n"))}">Copy</button></div>${lines.map((l) => `<div class="p">+ ${escapeHtml(l)}</div>`).join("") || '<div class="m">(remove the lines)</div>'}</div>`;
    });
  }

  async function click(e: MouseEvent) {
    const target = e.target as HTMLElement;
    const copy = target.closest("[data-copy]") as HTMLElement | null;
    if (copy) {
      e.stopPropagation();
      try {
        await navigator.clipboard.writeText(copy.dataset.copy ?? "");
        notify("Suggested change copied", "info");
      } catch {
        notify("The suggestion could not be copied");
      }
      return;
    }
    const link = target.closest(".md-link") as HTMLElement | null;
    if (link?.dataset.href) {
      e.preventDefault();
      e.stopPropagation();
      const href = link.dataset.href;
      try {
        await api.openPullRequestLink(data.url, href);
      } catch {
        // Links to other sites are not opened from a comment; they can be copied.
        try {
          await navigator.clipboard.writeText(href);
          notify(`Links to other sites are not opened from comments. Copied: ${href}`, "info");
        } catch {
          notify(`Links to other sites are not opened from comments: ${href}`);
        }
      }
    }
  }

  /** Opens a link of this pull request in the browser (its own host only). */
  function openLink(link: string) {
    api.openPullRequestLink(data.url, link).catch((e) => notify(String(e)));
  }

  const reviewLabel = (state: string) =>
    ({ APPROVED: "approved", CHANGES_REQUESTED: "requested changes", COMMENTED: "reviewed", DISMISSED: "review dismissed" })[state] ?? state.toLowerCase();
</script>

{#snippet note(n: PrNote, reply = false)}
  <div class="cmt" class:reply>
    <div class="who"><b>{n.author ?? "ghost"}</b>{#if n.bot}<span class="tag">bot</span>{/if} <span>{ago(n.created)}</span></div>
    <!-- Rendered from escaped text: no HTML of the comment reaches the page. -->
    <div class="body">{@html body(n)}</div>
  </div>
{/snippet}

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions, a11y_no_noninteractive_element_interactions -->
<aside class="panel" bind:this={el} onclick={click} aria-label="Pull request threads">
  <header>
    <div class="top">
      <b title={path ?? data.title}>{path === null ? `#${data.number} ${data.title}` : (path.split("/").pop() ?? path)}</b>
      <button class="x" onclick={onClose} title="Hide the threads" aria-label="Hide the threads">✕</button>
    </div>
    {#if path === null && scope}
      <div class="scope">In <b>{scope}</b> <button class="link" onclick={onClearScope}>Show all files</button></div>
    {/if}
    <span class="seg" role="tablist">
      <button role="tab" class:on={tab === "open"} onclick={() => (tab = "open")}>Open {lists.open.length + annotations.length}</button>
      <button role="tab" class:on={tab === "resolved"} onclick={() => (tab = "resolved")}>Resolved {lists.resolved.length}</button>
      <button role="tab" class:on={tab === "outdated"} onclick={() => (tab = "outdated")}>Outdated {lists.outdated.length}</button>
      {#if path === null && !scope}<button role="tab" class:on={tab === "conversation"} onclick={() => (tab = "conversation")}>Conversation {data.reviews.filter((r) => r.body.trim()).length + data.comments.length}</button>{/if}
    </span>
  </header>
  <div class="list">
    {#if tab === "conversation"}
      {#each [...data.reviews.filter((r) => r.body.trim() || r.state !== "COMMENTED").map((r) => ({ kind: "review" as const, at: r.submitted ?? "", r })), ...data.comments.map((c) => ({ kind: "comment" as const, at: c.created, c }))].sort((a, b) => a.at.localeCompare(b.at)) as item, i (i)}
        <div class="thread">
          {#if item.kind === "review"}
            <div class="where"><span class="state {item.r.state}">{item.r.author ?? "ghost"} {reviewLabel(item.r.state)}</span><span>{ago(item.r.submitted)}</span></div>
            {#if item.r.body.trim()}<div class="body">{@html body({ author: item.r.author, bot: item.r.bot, body: item.r.body, created: item.r.submitted ?? "", url: item.r.url })}</div>{/if}
          {:else}
            {@render note(item.c)}
          {/if}
        </div>
      {:else}
        <p class="empty">No reviews or comments on the pull request as a whole.</p>
      {/each}
    {:else}
      {#each sorted(lists[tab]) as { thread, index } (index)}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="thread" class:cur={index === current} class:dimmed={tab !== "open"} data-thread={index} onclick={() => onGo(index)}>
          <div class="where">
            <b>{where(thread)}</b>
            {#if thread.severity}<span class="sev {thread.severity}">{thread.severity}</span>{/if}
            {#if thread.comments[0]?.url}
              <button class="gh" onclick={(e) => { e.stopPropagation(); openLink(thread.comments[thread.comments.length - 1].url); }}
                title="Opens this thread on GitHub, where you can reply, resolve it or react">{thread.resolved || thread.outdated ? "Open on GitHub ↗" : "Reply on GitHub ↗"}</button>
            {/if}
          </div>
          {#each thread.comments as c, i (i)}{@render note(c, i > 0)}{/each}
        </div>
      {/each}
      {#if tab === "open"}
        {#each annotations as a, i (i)}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <div class="thread annotation" class:go={!!onAnnotation} onclick={() => onAnnotation?.(a)} title={onAnnotation ? "Show the line" : undefined}>
            <div class="where"><b>{#if a.path === ".github" || a.path === ""}{a.path || "repository"} · workflow{:else}{path === null ? `${a.path} · ` : ""}right {a.startLine}{a.endLine !== a.startLine ? `–${a.endLine}` : ""}{/if}</b><span class="sev {a.level}">{a.level === "failure" ? "Failure" : a.level === "warning" ? "Warning" : "Notice"}</span><span>{a.check}</span>{#if a.url}<button class="gh" onclick={(e) => { e.stopPropagation(); openLink(a.url!); }} title="Opens the check run on GitHub">Open check ↗</button>{/if}</div>
            <div class="body">{#if a.title}<b>{a.title}</b> · {/if}{a.message}</div>
          </div>
        {/each}
      {/if}
      {#if !lists[tab].length && !(tab === "open" && annotations.length)}
        <p class="empty">{tab === "open" ? "No open threads." : tab === "resolved" ? "No resolved threads." : "No outdated threads."}</p>
      {/if}
    {/if}
    {#if data.truncated}<p class="empty">Some comments are left out: the pull request has more than the app reads.</p>{/if}
  </div>
</aside>

<style>
  .panel { width: 340px; flex: none; display: flex; flex-direction: column; min-height: 0; border-left: 1px solid var(--line); background: var(--panel-2); font-size: 12px; }
  header { display: grid; gap: 5px; padding: 5px 8px 6px; border-bottom: 1px solid var(--line); background: var(--panel); min-width: 0; }
  .top { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .top b { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .seg { display: inline-flex; flex-wrap: wrap; }
  .seg button { font-size: 11px; padding: 0 6px; border-radius: 0; margin-left: -1px; }
  .seg button:first-child { border-radius: 4px 0 0 4px; margin-left: 0; }
  .seg button:last-child { border-radius: 0 4px 4px 0; }
  .seg button.on { background: var(--sel); border-color: var(--accent); position: relative; }
  .x { border: none; background: none; color: var(--dim); padding: 0 2px; }
  .list { overflow-y: auto; min-height: 0; flex: 1; }
  .thread { display: grid; gap: 6px; padding: 8px 10px; border-bottom: 1px solid var(--line); cursor: default; }
  .thread[data-thread], .thread.go { cursor: pointer; }
  .gh { margin-left: auto; border: none; background: none; color: var(--accent); padding: 0 2px; font-size: 11px; cursor: pointer; white-space: nowrap; }
  .gh:hover { text-decoration: underline; }
  .scope { font-size: 11.5px; color: var(--dim); }
  .scope b { color: var(--text); font-family: var(--mono); font-weight: 500; }
  .link { border: none; background: none; color: var(--accent); padding: 0 4px; cursor: pointer; font-size: 11.5px; }
  .thread.cur { background: var(--bg); box-shadow: inset 3px 0 0 var(--accent); }
  .thread.dimmed { opacity: 0.8; }
  .where { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; color: var(--dim); font-size: 11px; }
  .where b { color: var(--text); font-family: var(--mono); font-weight: 500; overflow-wrap: anywhere; }
  .sev { font-size: 10.5px; font-weight: 600; border-radius: 3px; padding: 0 5px; color: var(--bg); background: var(--dim); }
  .sev.Important, .sev.failure { background: var(--diff); }
  .sev.Nit, .sev.warning { background: var(--similar); }
  .sev.notice { background: var(--right-only); }
  .state { font-weight: 600; color: var(--text); }
  .state.APPROVED { color: #2d8a4e; }
  .state.CHANGES_REQUESTED { color: var(--diff); }
  .cmt { display: grid; gap: 2px; }
  .cmt.reply { margin-left: 10px; padding-left: 8px; border-left: 2px solid var(--line); }
  .who { font-size: 11.5px; }
  .who b { font-weight: 600; }
  .who span { color: var(--dim); }
  .tag { border: 1px solid var(--line); border-radius: 6px; padding: 0 4px; margin-left: 4px; font-size: 10px; color: var(--dim); }
  .body { line-height: 1.45; overflow-wrap: anywhere; min-width: 0; }
  .body :global(p) { margin: 0 0 6px; }
  .body :global(p:last-child) { margin-bottom: 0; }
  .body :global(.md-h) { font-weight: 600; }
  .body :global(code) { font-family: var(--mono); font-size: 11px; background: var(--panel); padding: 0 3px; border-radius: 3px; }
  .body :global(pre) { margin: 4px 0; padding: 6px; background: var(--panel); border-radius: 4px; overflow-x: auto; }
  .body :global(pre code) { background: none; padding: 0; }
  .body :global(ul), .body :global(ol) { margin: 2px 0 6px; padding-left: 18px; }
  .body :global(blockquote) { margin: 2px 0; padding-left: 8px; border-left: 2px solid var(--line); color: var(--dim); }
  .body :global(.md-link) { color: var(--accent); cursor: pointer; text-decoration: underline; }
  .body :global(.sugg) { font-family: var(--mono); font-size: 11px; border: 1px solid var(--line); border-radius: 4px; overflow: hidden; margin: 4px 0; }
  .body :global(.sugg div) { padding: 1px 6px; white-space: pre-wrap; }
  .body :global(.sugg .h) { background: var(--panel); color: var(--dim); font-family: var(--ui); display: flex; justify-content: space-between; align-items: center; }
  .body :global(.sugg .p) { background: var(--row-added); }
  .body :global(.sugg .m) { background: var(--row-changed); }
  .body :global(.copy) { font-size: 10.5px; padding: 0 6px; }
  .empty { margin: 0; padding: 12px 10px; color: var(--dim); }
</style>
