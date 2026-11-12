<script lang="ts">
  // Finishing the signed-in user's review: a summary, the decision, and the
  // pending comments it sends. One click on Submit; nothing is sent before.
  import { tick } from "svelte";
  import type { PrConversation } from "./api";
  import { intoReview, prConversations, unsent } from "./prConversation.svelte";
  import { ask } from "./confirm.svelte";

  let { data, session, onClose }: { data: PrConversation; session: number; onClose: () => void } = $props();

  type Event = "COMMENT" | "APPROVE" | "REQUEST_CHANGES";
  // svelte-ignore state_referenced_locally
  let summary = $state(unsent.get(data.url, "summary") || data.review?.body || "");
  let event = $state<Event>("COMMENT");
  let area = $state<HTMLTextAreaElement>();
  const busy = $derived(!!prConversations.busy[session]);
  const host = $derived.by(() => {
    try {
      return new URL(data.url).host;
    } catch {
      return "GitHub";
    }
  });
  const pending = $derived(
    data.threads.flatMap((t) =>
      t.comments.filter((c) => c.pending).map((c) => ({ where: `${t.path} · ${t.fileLevel || t.line === null ? "file" : `${t.side} ${t.startLine && t.startLine !== t.line ? `${t.startLine}–` : ""}${t.line}`}`, text: c.body.split("\n")[0], reply: t.comments[0] !== c })),
    ),
  );
  const count = $derived(Math.max(pending.length, data.review?.comments ?? 0));
  const why = $derived(
    event === "REQUEST_CHANGES" && !summary.trim()
      ? "Requesting changes needs a summary"
      : event === "COMMENT" && !summary.trim() && !count
        ? "Write a summary or add comments to comment"
        : null,
  );

  $effect(() => unsent.set(data.url, "summary", summary));
  $effect(() => {
    tick().then(() => area?.focus());
  });

  async function submit() {
    if (why || busy) return;
    if (await prConversations.write(session, { kind: "submit", review: intoReview(data), event, body: summary })) {
      unsent.set(data.url, "summary", "");
      onClose();
    }
  }

  async function discard() {
    if (!data.review) return;
    const ok = await ask(`Discard your pending review with ${count === 1 ? "1 comment" : `${count} comments`}? Nobody has seen them; they cannot be brought back.`, {
      title: "Discard review",
      kind: "warning",
      okLabel: "Discard review",
    });
    if (ok && (await prConversations.write(session, { kind: "discard", review: data.review.id }))) onClose();
  }

  function key(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void submit();
    }
  }

  const choices: { value: Event; label: string; detail: string }[] = [
    { value: "COMMENT", label: "Comment", detail: "Feedback without approving or requesting changes." },
    { value: "APPROVE", label: "Approve", detail: "Approve merging these changes." },
    { value: "REQUEST_CHANGES", label: "Request changes", detail: "Feedback that must be addressed before merging." },
  ];
</script>

<div class="back" role="presentation">
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="review-title" tabindex="-1" onkeydown={key}>
    <h2 id="review-title">Finish your review of #{data.number}</h2>
    <p class="sub">{count ? `${count} pending ${count === 1 ? "comment" : "comments"}, sent with the review` : "No pending comments: the review is the summary and the decision"}</p>
    <textarea bind:this={area} bind:value={summary} rows="4" placeholder="Summary (Markdown)" aria-label="Summary"></textarea>
    <div class="choices" role="radiogroup" aria-label="Decision">
      {#each choices as c (c.value)}
        {@const off = data.own && c.value !== "COMMENT"}
        <label class:off title={off ? "GitHub does not take approvals or change requests on your own pull request" : undefined}>
          <input type="radio" name="event" value={c.value} bind:group={event} disabled={off} />
          <b>{c.label}</b><span>{off ? "Not on your own pull request." : c.detail}</span>
        </label>
      {/each}
    </div>
    {#if pending.length}
      <div class="plist">
        {#each pending as p, i (i)}<div><b>{p.where}</b>{#if p.reply}<i>reply</i>{/if}<span>{p.text}</span></div>{/each}
      </div>
    {/if}
    <div class="foot">
      <span class="who">Posts as <b>{data.viewer}</b> on {host}</span>
      {#if data.review}<button class="danger" onclick={discard} disabled={busy}>Discard review…</button>{/if}
      <button onclick={onClose}>Keep reviewing</button>
      <button class="primary" onclick={submit} disabled={!!why || busy} title={why ?? "Ctrl+Enter"}>Submit review</button>
    </div>
  </div>
</div>

<style>
  .back { position: fixed; inset: 0; z-index: 150; display: grid; place-items: start center; padding-top: 10vh; background: rgb(0 0 0 / 0.3); }
  .dialog { width: min(560px, 92vw); display: grid; gap: 10px; padding: 16px 18px 14px; background: var(--panel); border: 1px solid var(--line); border-radius: 8px; box-shadow: 0 20px 60px rgb(0 0 0 / 0.35); font-size: 12.5px; max-height: 80vh; overflow: auto; }
  h2 { margin: 0; font-size: 14px; }
  .sub { margin: -6px 0 0; color: var(--dim); font-size: 12px; }
  textarea { resize: vertical; min-height: 70px; padding: 6px 8px; font-family: var(--mono); font-size: 12px; line-height: 1.5; background: var(--bg); color: var(--text); border: 1px solid var(--line); border-radius: 4px; }
  .choices { display: grid; gap: 6px; }
  label { display: grid; grid-template-columns: 18px auto 1fr; gap: 2px 6px; align-items: baseline; }
  label span { grid-column: 2 / 4; color: var(--dim); font-size: 11.5px; }
  label.off { opacity: 0.55; }
  .plist { display: grid; gap: 3px; font-size: 12px; border-top: 1px solid var(--line); padding-top: 8px; }
  .plist div { display: flex; gap: 8px; min-width: 0; }
  .plist b { font-family: var(--mono); font-weight: 500; white-space: nowrap; }
  .plist i { color: var(--dim); }
  .plist span { color: var(--dim); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .foot { display: flex; flex-wrap: wrap; gap: 8px; justify-content: flex-end; align-items: center; border-top: 1px solid var(--line); padding-top: 10px; }
  .who { margin-right: auto; color: var(--dim); font-size: 11.5px; }
  .primary { background: var(--accent); border-color: var(--accent); color: #fff; }
  .primary:disabled { opacity: 0.55; }
  .danger { color: var(--diff); }
</style>
