<script lang="ts">
  import { api, type PrAnnotation, type PrConversation, type PrNote, type PrThread, type PrWrite } from "./api";
  import { commentHtml } from "./markdown";
  import { ago, intoReview, linesApply, linesBlocked, prConversations } from "./prConversation.svelte";
  import { notify } from "./toast.svelte";
  import { ask } from "./confirm.svelte";
  import { tick } from "svelte";
  import Composer, { type ComposerAction } from "./Composer.svelte";

  let {
    data,
    session = null,
    path = null,
    scope = null,
    current = null,
    onGo,
    onAnnotation,
    onClearScope,
    onClose,
  }: {
    data: PrConversation;
    /** The comparison session, for writing; null shows the conversation read-only. */
    session?: number | null;
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

  const PAGE = 50;
  let threadPage = $state(0), annotationPage = $state(0);
  const view = $derived(`${data.url}:${path}:${scope}:${tab}`);
  $effect(() => { void view; threadPage = annotationPage = 0; });
  const ordered = $derived(sorted(tab === "conversation" ? [] : lists[tab]));
  const threadStart = $derived(Math.min(threadPage, Math.max(0, Math.ceil(ordered.length / PAGE) - 1)) * PAGE);
  const annotationStart = $derived(Math.min(annotationPage, Math.max(0, Math.ceil(annotations.length / PAGE) - 1)) * PAGE);
  const visibleThreads = $derived(ordered.slice(threadStart, threadStart + PAGE));
  const visibleAnnotations = $derived(annotations.slice(annotationStart, annotationStart + PAGE));
  $effect(() => {
    if (current === null || !el) return;
    const t = data.threads[current];
    if (t && !t.resolved && !t.outdated) tab = "open";
    const position = sorted(tab === "conversation" ? [] : lists[tab]).findIndex((entry) => entry.index === current);
    if (position >= 0) threadPage = Math.floor(position / PAGE);
    tick().then(() => el?.querySelector(`[data-thread="${current}"]`)?.scrollIntoView({ block: "nearest" }));
  });

  function where(t: PrThread): string {
    const file = path === null ? `${t.path} · ` : "";
    if (t.fileLevel) return `${file}on the file`;
    if (!placed(t)) return `${file}${t.outdated ? "was " : ""}line ${t.line ?? t.originalLine ?? "?"}${t.outdated ? "" : " on the newer head"}`;
    const lines = t.startLine && t.startLine !== t.line ? `${t.startLine}–${t.line}` : `${t.line}`;
    return `${file}${t.side} ${lines}`;
  }

  /** A comment as safe HTML, with its suggested changes shown as added lines. */
  const body = (note: PrNote): string => commentHtml(note.body);

  // ---- Writing ----
  const writable = $derived(session !== null && !!data.viewer && data.canComment);
  const readState = $derived(session === null ? undefined : prConversations.get(session));
  const busy = $derived(session !== null && (!!prConversations.busy[session] || !!readState?.loading || !!readState?.error));
  const as = $derived(data.viewer ? ` as ${data.viewer}` : "");
  /** Keep the original thread identity through refreshes and deletion. */
  let replying = $state<PrThread | null>(null);
  const missingReply = $derived(replying && !data.threads.some((t) => t.id === replying!.id) ? replying : null);
  let editing = $state<string | null>(null);
  const write = (change: PrWrite) => (session === null ? Promise.resolve(false) : prConversations.write(session, change));

  // R in the comparison: the reply field of the thread at the selected row.
  $effect(() => {
    const r = prConversations.replyTo;
    if (!r || r.session !== session) return;
    prConversations.replyTo = null;
    const t = data.threads[r.thread];
    if (t?.canReply && t.id) {
      if (t.resolved) tab = "resolved";
      replying = t;
    } else notify(t ? "You cannot reply to this thread" : "No thread at the selected line", "info");
  });

  /** The new comment being written in this panel's file. */
  const target = $derived.by(() => {
    const c = prConversations.composer;
    return c && c.session === session && (path === null || c.path === path) ? c : null;
  });

  function lines(start: number, end: number) {
    return end === 0 ? "the file" : start && start !== end ? `lines ${start}–${end}` : `line ${end}`;
  }

  function commentActions(): ComposerAction[] {
    const t = target!;
    const send = (review: boolean) => async (text: string) => {
      const ok = await write({ kind: "comment", path: t.path, side: t.side, start: t.start, end: t.end, body: text, head: data.pinnedHead, review: review ? intoReview(data) : null });
      if (ok) prConversations.composer = null;
      return ok;
    };
    if (data.review)
      return [{ label: "Add to review", primary: true, review: true, title: "Adds the comment to your pending review: only you see it until you submit", run: send(true) }];
    return [
      { label: "Comment now", title: `Posts this one comment now${as}; everyone subscribed is notified`, run: send(false) },
      { label: "Start a review", primary: true, review: true, title: "Starts your pending review with this comment: only you see it until you submit", run: send(true) },
    ];
  }

  function replyActions(t: PrThread): ComposerAction[] {
    const comment = t.comments.find((c) => c.databaseId !== null)?.databaseId ?? 0;
    const send = (review: boolean, resolve = false) => async (text: string) => {
      let ok = await write({ kind: "reply", thread: t.id, comment, body: text, review: review ? intoReview(data) : null });
      if (ok && resolve) ok = await write({ kind: "resolve", thread: t.id, resolved: true });
      if (ok && replying?.id === t.id) replying = null;
      return ok;
    };
    if (data.review)
      return [{ label: "Add to review", primary: true, review: true, title: "Adds the reply to your pending review: sent when you submit it", run: send(true) }];
    return [
      ...(t.canResolve && !t.resolved && comment ? [{ label: "Reply and resolve", title: `Posts the reply now${as} and resolves the thread`, run: send(false, true) }] : []),
      { label: "Start a review", review: true, title: "Starts your pending review with this reply: only you see it until you submit", run: send(true) },
      ...(comment ? [{ label: "Reply", primary: true, title: `Posts the reply now${as}`, run: send(false) }] : []),
    ];
  }

  async function remove(n: PrNote) {
    const ok = await ask(n.pending ? "Delete this comment from your pending review?" : "Delete this comment on GitHub? Everyone sees that it was deleted, and it cannot be undone.", {
      title: "Delete comment",
      kind: "warning",
      okLabel: "Delete",
    });
    if (ok) await write({ kind: "delete", comment: n.id });
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

{#snippet note(n: PrNote, reply = false, inPending = false)}
  <div class="cmt" class:reply>
    <div class="who">
      <b>{n.author ?? "ghost"}</b>{#if n.bot}<span class="tag">bot</span>{/if}
      {#if n.pending && !inPending}<span class="pending" title="In your pending review: only you see it until you submit">Pending</span>{:else if !n.pending}<span>{ago(n.created)}</span>{/if}
      {#if writable && editing !== n.id && (n.canUpdate || n.canDelete) && n.id}
        <span class="own">
          {#if n.canUpdate}<button class="link" onclick={(e) => { e.stopPropagation(); editing = n.id; }}>Edit</button>{/if}
          {#if n.canDelete}<button class="link del" onclick={(e) => { e.stopPropagation(); void remove(n); }}>Delete</button>{/if}
        </span>
      {/if}
    </div>
    {#if editing === n.id && session !== null}
      <Composer heading="Edit your comment" url={data.url} target="edit:{n.id}" initial={n.body} {busy}
        actions={[{ label: "Save", primary: true, title: n.pending ? "Saves the comment in your pending review" : "Saves the change on GitHub; GitHub marks the comment as edited", run: async (text) => {
          const ok = await write({ kind: "edit", comment: n.id, body: text });
          if (ok) editing = null;
          return ok;
        } }]}
        onCancel={() => (editing = null)} />
    {:else}
      <!-- Rendered from escaped text: no HTML of the comment reaches the page. -->
      <div class="body">{@html body(n)}</div>
    {/if}
  </div>
{/snippet}

<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions, a11y_no_noninteractive_element_interactions -->
<aside class="panel" bind:this={el} onclick={click} aria-label="Pull request threads">
  <header>
    <div class="top">
      <b title={path ?? data.title}>{path === null ? `#${data.number} ${data.title}` : (path.split("/").pop() ?? path)}</b>
      <button class="x" onclick={onClose} title="Hide the threads" aria-label="Hide the threads">✕</button>
    </div>
    {#if path !== null && writable && data.files[path] && !target}
      {@const blocked = linesBlocked(data)}
      <div class="scope"><button class="link" disabled={!!blocked} title={blocked ?? "A comment on the file as a whole, not on lines"}
        onclick={() => (prConversations.composer = { session: session!, path: path!, side: "right", start: 0, end: 0 })}>Comment on the file</button>
        <span>· select lines and press C to comment on them</span></div>
    {/if}
    {#if path === null && scope}
      <div class="scope">In <b>{scope}</b> <button class="link" onclick={onClearScope}>Show all files</button></div>
    {/if}
    <span class="seg" role="tablist">
      <button role="tab" class:on={tab === "open"} onclick={() => (tab = "open")} title="Open threads, and the findings of CI checks">Open {lists.open.length}{#if annotations.length}{" "}· ▲ {annotations.length}{/if}</button>
      <button role="tab" class:on={tab === "resolved"} onclick={() => (tab = "resolved")}>Resolved {lists.resolved.length}</button>
      <button role="tab" class:on={tab === "outdated"} onclick={() => (tab = "outdated")}>Outdated {lists.outdated.length}</button>
      {#if path === null && !scope}<button role="tab" class:on={tab === "conversation"} onclick={() => (tab = "conversation")}>Conversation {data.reviews.filter((r) => r.body.trim()).length + data.comments.length}</button>{/if}
    </span>
  </header>
  <div class="list">
    {#if target && writable}
      <div class="thread new">
        <Composer heading="New comment on {path === null ? `${target.path} · ` : ""}{target.end ? `${target.side} ` : ""}{lines(target.start, target.end)}" url={data.url}
          target="new:{target.path}:{target.side}:{target.start}-{target.end}" suggestion={target.side === "right" && target.end ? (target.suggestion ?? null) : null}
          {busy} actions={commentActions()} onCancel={() => (prConversations.composer = null)} />
      </div>
    {/if}
    {#if tab === "conversation"}
      {#each [...data.reviews.filter((r) => r.body.trim() || r.state !== "COMMENTED").map((r) => ({ kind: "review" as const, at: r.submitted ?? "", r })), ...data.comments.map((c) => ({ kind: "comment" as const, at: c.created, c }))].sort((a, b) => a.at.localeCompare(b.at)) as item, i (i)}
        <div class="thread">
          {#if item.kind === "review"}
            <div class="where"><span class="state {item.r.state}">{item.r.author ?? "ghost"} {reviewLabel(item.r.state)}</span><span>{ago(item.r.submitted)}</span></div>
            {#if item.r.body.trim()}<div class="body">{@html commentHtml(item.r.body)}</div>{/if}
          {:else}
            {@render note(item.c)}
          {/if}
        </div>
      {:else}
        <p class="empty">No reviews or comments on the pull request as a whole.</p>
      {/each}
    {:else}
      {#if missingReply}
        <section class="orphan" role="status">
          <p>The original reply thread is no longer available. Your draft is kept; it will not be sent to another thread.</p>
          {#key missingReply.id}<Composer heading="Unsent reply · original thread unavailable" url={data.url} target="reply:{missingReply.id}" actions={[]} rows={3} onCancel={() => (replying = null)} />{/key}
        </section>
      {/if}
      {#if ordered.length > PAGE}
        <nav class="paging" aria-label="Review thread pages">
          <button class="sm" disabled={threadStart === 0} onclick={() => (threadPage = threadStart / PAGE - 1)}>Previous threads</button>
          <span>{threadStart + 1}–{Math.min(threadStart + PAGE, ordered.length)} of {ordered.length} threads</span>
          <button class="sm" disabled={threadStart + PAGE >= ordered.length} onclick={() => (threadPage = threadStart / PAGE + 1)}>Next threads</button>
        </nav>
      {/if}
      {#each visibleThreads as { thread, index } (thread.id || thread.comments[0]?.id || index)}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <div class="thread" class:cur={index === current} class:dimmed={tab !== "open"} data-thread={index} onclick={() => onGo(index)}>
          <div class="where">
            <b>{where(thread)}</b>
            {#if thread.pending}<span class="pending">Pending</span>{/if}
            {#if thread.severity}<span class="sev {thread.severity}">{thread.severity}</span>{/if}
            {#if thread.comments[0]?.url}
              <button class="gh" onclick={(e) => { e.stopPropagation(); openLink(thread.comments[thread.comments.length - 1].url); }}
                title="Opens this thread on GitHub">{thread.resolved || thread.outdated || writable ? "Open on GitHub ↗" : "Reply on GitHub ↗"}</button>
            {/if}
          </div>
          {#each thread.comments as c, i (i)}{@render note(c, i > 0, thread.pending)}{/each}
          {#if writable && thread.id}
            {#if replying?.id === thread.id}
              <Composer heading="Reply{as}" url={data.url} target="reply:{thread.id}" {busy} rows={3} actions={replyActions(thread)} onCancel={() => (replying = null)} />
            {:else if !thread.pending || thread.canResolve || thread.canUnresolve}
              <div class="acts">
                {#if thread.canReply}<button class="replybox" onclick={(e) => { e.stopPropagation(); replying = thread; }} disabled={busy}>Reply…</button>{/if}
                {#if thread.canResolve && !thread.resolved}<button class="sm" onclick={(e) => { e.stopPropagation(); void write({ kind: "resolve", thread: thread.id, resolved: true }); }} disabled={busy} title="Marks the conversation as resolved on GitHub">✓ Resolve</button>{/if}
                {#if thread.canUnresolve && thread.resolved}<button class="sm" onclick={(e) => { e.stopPropagation(); void write({ kind: "resolve", thread: thread.id, resolved: false }); }} disabled={busy}>Unresolve</button>{/if}
              </div>
            {/if}
          {/if}
        </div>
      {/each}
      {#if tab === "open"}
        {#if annotations.length > PAGE}
          <nav class="paging" aria-label="CI finding pages">
            <button class="sm" disabled={annotationStart === 0} onclick={() => (annotationPage = annotationStart / PAGE - 1)}>Previous findings</button>
            <span>{annotationStart + 1}–{Math.min(annotationStart + PAGE, annotations.length)} of {annotations.length} CI findings</span>
            <button class="sm" disabled={annotationStart + PAGE >= annotations.length} onclick={() => (annotationPage = annotationStart / PAGE + 1)}>Next findings</button>
          </nav>
        {/if}
        {#each visibleAnnotations as a, i (annotationStart + i)}
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
  .paging { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; padding: 8px; border-block: 1px solid var(--line); font-size: 11px; }
  .orphan { padding: 8px; border: 1px solid var(--similar); }
  .orphan p { font-size: 12px; margin: 0 0 8px; }
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
  .thread.new { background: var(--bg); }
  .pending { font-size: 9.5px; font-weight: 600; letter-spacing: 0.04em; text-transform: uppercase; color: var(--similar); border: 1px solid currentColor; border-radius: 3px; padding: 0 4px; margin-left: 4px; }
  .own { float: right; }
  .own .link { font-size: 11px; padding: 0 2px; }
  .own .del { color: var(--diff); }
  .acts { display: flex; gap: 6px; align-items: center; }
  .replybox { flex: 1; text-align: left; color: var(--dim); background: var(--bg); font-size: 11.5px; padding: 2px 8px; cursor: text; }
  .sm { font-size: 11px; padding: 1px 7px; white-space: nowrap; }
</style>
