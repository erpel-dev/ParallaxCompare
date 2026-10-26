<script lang="ts">
  import { tick } from "svelte";
  import SidePicker from "./SidePicker.svelte";
  import PathInput from "./PathInput.svelte";
  import {
    pickerFor,
    restorePicker,
    shapeMismatch,
    toSpec,
    usable,
    type PickerValue,
    type PickerPreview,
  } from "./sourcePicker";
  import {
    api,
    isPullRequestUrl,
    sourceName,
    sourceText,
    type MergeRequest,
    type PullRequestComparison,
    type PullRequestInfo,
    type SourceOptions,
    type SourceSpec,
  } from "./api";
  import { comparisonKey, forgetComparison, pinComparison, recentComparisons, type SavedComparison } from "./recent";

  let {
    onCompare,
    onMerge,
    onConflicts,
  }: {
    onCompare: (
      left: SourceSpec,
      right: SourceSpec,
      options: SourceOptions,
      mergeBase: boolean,
      pullRequest?: PullRequestInfo,
    ) => void | Promise<void>;
    /** Opens a 3-way merge of files. */
    onMerge: (request: MergeRequest) => void | Promise<void>;
    /** Opens the list of conflicts in a repository. */
    onConflicts: (dir: string) => void | Promise<void>;
  } = $props();
  const STORAGE_KEY = "parallax.newCompare";
  function load() {
    const defaults = {
      left: { path: "" } as PickerValue,
      right: { path: "" } as PickerValue,
      options: { expandArchives: true, ignored: "mark" } as SourceOptions,
      mergeBase: false,
      prUrl: "",
    };
    try {
      const saved = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null");
      if (!saved || typeof saved !== "object") return defaults;
      const l = restorePicker(saved.left),
        r = restorePicker(saved.right);
      const prUrl =
        typeof saved.prUrl === "string" && isPullRequestUrl(saved.prUrl)
          ? saved.prUrl
          : ([l.path, r.path].find(isPullRequestUrl) ?? "");
      return {
        left: isPullRequestUrl(l.path) ? { path: "" } : l,
        right: isPullRequestUrl(r.path) ? { path: "" } : r,
        options: {
          expandArchives: saved.options?.expandArchives !== false,
          // Before ignored files were shown by default, the setting was `respectGitignore`; it starts over.
          ignored: saved.options?.ignored === "hide" ? ("hide" as const) : ("mark" as const),
        },
        mergeBase: saved.mergeBase === true,
        prUrl,
      };
    } catch {
      return defaults;
    }
  }
  const initial = load();
  let left = $state(initial.left),
    right = $state(initial.right);
  let leftPreview = $state<PickerPreview | null>(null),
    rightPreview = $state<PickerPreview | null>(null);
  let options = $state(initial.options),
    mergeBase = $state(initial.mergeBase);
  let prUrl = $state(initial.prUrl),
    prMode = $state(!!initial.prUrl);
  let prResult = $state<PullRequestComparison | null>(null),
    resolvedFor = $state("");
  let busy = $state<"resolve" | "compare" | null>(null),
    error = $state("");
  let prInput = $state<HTMLInputElement>(),
    compareButton = $state<HTMLButtonElement>();
  let optionsOpen = $state(false);
  /** A 3-way merge of files, or a repository's conflicts, instead of a comparison. */
  let extra = $state<"merge" | "conflicts" | null>(null);
  let merge = $state({ local: "", base: "", remote: "", output: "" });
  let conflictsDir = $state("");
  const extraReady = $derived(
    extra === "merge" ? !!(merge.local.trim() && merge.remote.trim()) : extra === "conflicts" ? !!conflictsDir.trim() : false,
  );
  const mismatch = $derived(shapeMismatch(leftPreview, rightPreview));
  const ready = $derived(usable(leftPreview, left) && usable(rightPreview, right) && !mismatch);
  const bothGit = $derived(
    ready && [leftPreview, rightPreview].every((p) => p?.kind === "gitRev" || p?.kind === "remote"),
  );
  const resolved = $derived(prResult && resolvedFor === prUrl.trim() ? prResult : null);
  const prValid = $derived(isPullRequestUrl(prUrl));
  const prHost = $derived.by(() => {
    try {
      return new URL(prUrl).hostname;
    } catch {
      return "";
    }
  });
  $effect(() => {
    left.path;
    right.path;
    prUrl;
    error = "";
  });
  function remember() {
    try {
      localStorage.setItem(
        STORAGE_KEY,
        JSON.stringify({ left, right, options, mergeBase, prUrl: prMode ? prUrl : "" }),
      );
    } catch {
      /* Remembering the last comparison is a convenience only. */
    }
  }
  function swap() {
    [left, right] = [right, left];
    [leftPreview, rightPreview] = [rightPreview, leftPreview];
  }
  async function enterPR(url: string) {
    prUrl = url;
    prMode = true;
    prResult = null;
    error = "";
    await tick();
    prInput?.focus();
    prInput?.setSelectionRange(url.length, url.length);
  }
  async function backToPair() {
    prMode = false;
    prUrl = "";
    prResult = null;
    error = "";
    await tick();
    document.querySelector<HTMLInputElement>('.page:not([hidden]) input[aria-label="Left path"]')?.focus();
  }
  let recent = $state(recentComparisons());

  const title = (c: SavedComparison, depth: number) =>
    c.pullRequest
      ? `PR #${c.pullRequest.number}: ${c.pullRequest.title}`
      : `${sourceName(c.left, depth)} ↔ ${sourceName(c.right, depth)}${c.mergeBase ? " (merge-base)" : ""}`;

  /** Titles for the recent list, naming parent folders where two would otherwise read the same. */
  const titles = $derived.by(() => {
    const depth = recent.map(() => 1);
    for (let round = 0; round < 4; round++) {
      const shown = recent.map((c, i) => title(c, depth[i]));
      let clash = false;
      shown.forEach((t, i) => {
        if (shown.indexOf(t) !== shown.lastIndexOf(t)) {
          depth[i]++;
          clash = true;
        }
      });
      if (!clash) break;
    }
    return recent.map((c, i) => title(c, depth[i]));
  });

  function ago(ms: number): string {
    const s = Math.max(0, (Date.now() - ms) / 1000);
    if (s < 60) return "just now";
    if (s < 3600) return `${Math.floor(s / 60)} min ago`;
    if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
    const days = Math.floor(s / 86400);
    return days === 1 ? "yesterday" : `${days} days ago`;
  }

  function edit(c: SavedComparison) {
    if (c.pullRequest) enterPR(c.pullRequest.url);
    else {
      prMode = false;
      prUrl = "";
      left = pickerFor(c.left);
      right = pickerFor(c.right);
      leftPreview = rightPreview = null;
    }
    options = { ...c.options, ignored: c.options.ignored ?? "mark" };
    mergeBase = c.mergeBase;
  }
  function pin(c: SavedComparison) {
    pinComparison(comparisonKey(c), !c.pinned);
    recent = recentComparisons();
  }
  function forget(c: SavedComparison) {
    forgetComparison(comparisonKey(c));
    recent = recentComparisons();
  }
  async function openRecent(c: SavedComparison) {
    if (busy) return;
    busy = "compare";
    error = "";
    try {
      await onCompare(c.left, c.right, { ...c.options }, c.mergeBase, c.pullRequest);
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }
  /** Drops fill the form so detected types can be reviewed before Compare. */
  export async function fill(leftPath?: string, rightPath?: string) {
    const prs = [leftPath, rightPath].filter((p): p is string => !!p && isPullRequestUrl(p));
    if (prs.length) {
      if (new Set(prs).size > 1) {
        error = "Drop one PR URL; it supplies both comparison sides.";
        return;
      }
      await enterPR(prs[0]);
      return;
    }
    prMode = false;
    prUrl = "";
    if (leftPath) left = { path: leftPath };
    if (rightPath) right = { path: rightPath };
  }
  async function enterExtra(which: "merge" | "conflicts" | null) {
    extra = which;
    error = "";
    await tick();
    document.querySelector<HTMLInputElement>(".page:not([hidden]) .extra input")?.focus();
  }
  async function submitExtra() {
    if (busy || !extraReady) return;
    error = "";
    busy = "compare";
    const local = (path: string): SourceSpec => ({ type: "local", path: path.trim() });
    try {
      if (extra === "merge") {
        await onMerge({
          kind: "sources",
          base: merge.base.trim() ? local(merge.base) : null,
          local: local(merge.local),
          remote: local(merge.remote),
          output: (merge.output.trim() || merge.local).trim(),
        });
      } else {
        await onConflicts(conflictsDir.trim());
      }
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
  }
  async function submit() {
    if (extra) return submitExtra();
    if (busy || (prMode ? !prValid : !ready)) return;
    error = "";
    remember();
    busy = prMode && !resolved ? "resolve" : "compare";
    try {
      if (prMode) {
        if (!resolved) {
          const input = prUrl.trim();
          prResult = await api.resolvePullRequest(input);
          resolvedFor = input;
        } else {
          const { left: l, right: r, url, title, number } = resolved;
          await onCompare(l, r, { ...options }, false, { url, title, number });
        }
      } else {
        const [l, r] = await Promise.all([toSpec(left), toSpec(right)]);
        await onCompare(l, r, { ...options }, l.type === "gitRev" && r.type === "gitRev" && mergeBase);
      }
    } catch (e) {
      error = String(e);
    } finally {
      busy = null;
    }
    if (prMode && resolved) {
      await tick();
      compareButton?.focus();
    }
  }
</script>

<div class="wrap">
  <form
    class="dialog"
    onsubmit={(e) => {
      e.preventDefault();
      submit();
    }}
  >
    <fieldset disabled={!!busy}>
      {#if extra}
        <button type="button" class="link back" onclick={() => enterExtra(null)}>← Back to two sources</button>
        {#if extra === "merge"}
          <header class="heading">
            <div>
              <h2>Merge three files</h2>
              <p>Combine the changes local and remote made to a common base, and resolve what conflicts.</p>
            </div>
            <span class="auto">3-way merge</span>
          </header>
          <div class="extra grid">
            <div class="field"><span>Local</span><PathInput bind:value={merge.local} label="Local" placeholder="Local file (yours)" onEnter={submit} /></div>
            <div class="field"><span>Base</span><PathInput bind:value={merge.base} label="Base" placeholder="Common ancestor (optional)" onEnter={submit} /></div>
            <div class="field"><span>Remote</span><PathInput bind:value={merge.remote} label="Remote" placeholder="Remote file (theirs)" onEnter={submit} /></div>
            <div class="field wide"><span>Result</span><PathInput bind:value={merge.output} label="Result" placeholder="Where the result goes; the local file when empty" onEnter={submit} /></div>
          </div>
          <p class="note">Nothing is written until you save the result. Without a base, every difference is a conflict.</p>
        {:else}
          <header class="heading">
            <div>
              <h2>Resolve conflicts</h2>
              <p>List the conflicted files of a merge, rebase, cherry-pick or revert in progress.</p>
            </div>
            <span class="auto">Git</span>
          </header>
          <div class="extra">
            <div class="field"><span>Repository</span><PathInput bind:value={conflictsDir} label="Repository" placeholder="A folder in the repository" onEnter={submit} /></div>
          </div>
        {/if}
      {:else if prMode}
        <button type="button" class="link back" onclick={backToPair}>← Back to two sources</button>
        <header class="heading">
          <div>
            <h2>{resolved ? "Review pull request" : "Pull request detected"}</h2>
            <p>One URL supplies both sides of the comparison.</p>
          </div>
          <span class="auto">Read-only</span>
        </header>
        <div class="pr-banner">
          <strong
            >{resolved ? `#${resolved.number} · ${resolved.title}` : "Compare the PR head with its merge base"}</strong
          ><small
            >Resolve with GitHub CLI to preview the exact revisions. Going back restores your previous sources.</small
          >
        </div>
        <PathInput
          bind:value={prUrl}
          bind:input={prInput}
          label="Pull request URL"
          placeholder="https://github.com/owner/repo/pull/123"
          onEnter={submit}
        />
        <div class="pr-meta">
          <span>{prHost === "github.com" ? "GitHub" : "GitHub Enterprise"}</span><span>{prHost}</span><span
            >Uses gh + git and your existing authentication</span
          >
        </div>
        {#if !prValid}<p class="form-error" role="status">
            Enter an HTTPS GitHub or GitHub Enterprise pull request URL.
          </p>{/if}
        {#if busy === "resolve"}<p class="loading" role="status">Resolving pull request and fetching revisions…</p>{/if}
        {#if resolved}
          <div class="pr-pair">
            <section>
              <small>LEFT · MERGE BASE</small><strong>Common ancestor</strong><code
                >{resolved.left.type === "gitRev" ? resolved.left.rev : sourceText(resolved.left)}</code
              ><span>Read-only Git revision</span>
            </section>
            <div class="pr-arrow">→</div>
            <section class="right">
              <small>RIGHT · PR HEAD</small><strong>Latest PR commit</strong><code
                >{resolved.right.type === "gitRev" ? resolved.right.rev : sourceText(resolved.right)}</code
              ><span>Read-only Git revision</span>
            </section>
          </div>
          <p class="pr-help">
            Shows changes introduced by this PR. The comparison opens locally; it does not post a review.
          </p>
        {/if}
      {:else}
        <header class="heading">
          <div>
            <h2>New comparison</h2>
            <p>Paste or browse. We’ll identify each source.</p>
          </div>
          <span class="auto">Automatic detection</span>
        </header>
        <div class="sides">
          <SidePicker
            title="Left"
            bind:value={left}
            bind:preview={leftPreview}
            hideIgnored={options.ignored === "hide"}
            onEnter={submit}
            onPullRequest={enterPR}
          />
          <button type="button" class="swap" title="Swap sides" aria-label="Swap left and right sources" onclick={swap}
            >⇄</button
          >
          <SidePicker
            title="Right"
            bind:value={right}
            bind:preview={rightPreview}
            hideIgnored={options.ignored === "hide"}
            onEnter={submit}
            onPullRequest={enterPR}
          />
        </div>
        {#if bothGit}<div class="merge">
            <label title="Replace the left revision by its merge-base with the right one"
              ><input type="checkbox" bind:checked={mergeBase} /> Compare from the common ancestor (merge base)</label
            ><small
              >{mergeBase
                ? "Left will use the common ancestor; right stays at its selected revision."
                : "Use the selected revisions, or show only changes introduced on the right."}</small
            >
          </div>{/if}
        {#if mismatch}<p class="form-error mismatch" role="alert">
            These sources have different shapes: one is a file and the other is a folder or archive. Choose two files or
            two containers.
          </p>{/if}
        <div class="modes">
          <button type="button" class="link" onclick={() => enterExtra("merge")}>Merge three files…</button>
          <button type="button" class="link" onclick={() => enterExtra("conflicts")}>Resolve conflicts in a repository…</button>
        </div>
      {/if}
      {#if !extra}<details class="options" bind:open={optionsOpen}>
        <summary
          >Comparison options <span
            >· {options.expandArchives ? "Archives expanded" : "Archives kept as files"} · {options.ignored === "hide"
              ? "Ignored files hidden"
              : "Ignored files marked"}</span
          ></summary
        >
        <div class="opts">
          <label><input type="checkbox" bind:checked={options.expandArchives} /> Expand archives inside folders</label
          ><label
            ><input
              type="checkbox"
              checked={options.ignored === "hide"}
              onchange={(e) => (options.ignored = e.currentTarget.checked ? "hide" : "mark")}
            /> Hide files ignored by Git
            <small>otherwise they are compared and marked “ignored”</small></label
          >
        </div>
      </details>{/if}
      {#if error}<p class="form-error" role="alert">{error}</p>{/if}
      <footer class="form-footer">
        <span class="tip"
          >{extra
            ? extra === "merge"
              ? "Local and remote are required."
              : "The list reads Git's index; nothing changes until you resolve a file."
            : prMode
            ? resolved
              ? "Both revisions are ready to compare."
              : "Access is checked when you choose Resolve PR."
            : "Drop one or two paths, or paste a GitHub PR URL into either source."}</span
        ><button
          type="submit"
          class="primary"
          bind:this={compareButton}
          disabled={!!busy || (extra ? !extraReady : prMode ? !prValid : !ready)}
          >{extra
            ? busy
              ? "Opening…"
              : extra === "merge"
                ? "Merge →"
                : "Show conflicts →"
            : busy === "resolve"
            ? "Resolving…"
            : busy
              ? "Opening…"
              : prMode
                ? resolved
                  ? "Compare PR →"
                  : error
                    ? "Retry"
                    : "Resolve PR →"
                : "Compare →"}</button
        >
      </footer>
    </fieldset>
  </form>
  {#if recent.length}
    <section class="recent" aria-label="Recent comparisons">
      <header>Recent comparisons</header>
      {#each recent as c, i (comparisonKey(c))}
        <div class="entry">
          <button
            class="open"
            onclick={() => openRecent(c)}
            disabled={!!busy}
            title="Compare again with the same options"
          >
            <span class="names">{titles[i]}</span>
            <span class="paths mono">{sourceText(c.left)} ↔ {sourceText(c.right)}</span>
          </button>
          <span class="when">{c.pinned ? "pinned" : ago(c.used)}</span>
          <button
            class="icon"
            class:pinned={c.pinned}
            onclick={() => pin(c)}
            title={c.pinned ? "Unpin" : "Pin: keep at the top"}>{c.pinned ? "★" : "☆"}</button
          >
          <button class="icon" onclick={() => edit(c)} title="Put into the form above to change it">✎</button>
          <button class="icon" onclick={() => forget(c)} title="Remove from the list">✕</button>
        </div>
      {/each}
    </section>
  {/if}
</div>

<style>
  .wrap {
    height: 100%;
    overflow: auto;
    padding: 32px 20px;
    background: var(--panel-2);
  }
  .dialog {
    max-width: 1100px;
    margin: 0 auto;
    background: var(--input);
    border: 1px solid var(--line);
    border-radius: 10px;
    box-shadow: 0 6px 25px #0001;
    padding: 25px 28px 20px;
  }
  fieldset {
    border: 0;
    padding: 0;
    margin: 0;
    min-width: 0;
  }
  .heading {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 18px;
    margin-bottom: 22px;
  }
  h2 {
    font-size: 21px;
    letter-spacing: -0.4px;
    margin: 0;
    font-weight: 650;
  }
  .heading p {
    color: var(--dim);
    margin: 5px 0 0;
    font-size: 12px;
  }
  .auto {
    color: var(--accent);
    background: var(--panel-2);
    border: 1px solid var(--line);
    padding: 4px 8px;
    border-radius: 5px;
    font-size: 10px;
  }
  .sides {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr);
    gap: 12px;
    align-items: stretch;
  }
  .swap {
    align-self: center;
    border-radius: 50%;
    padding: 0;
    width: 30px;
    height: 30px;
    color: var(--dim);
    font-size: 17px;
  }
  .options {
    border-top: 1px solid var(--line);
    margin-top: 18px;
    padding-top: 14px;
    color: var(--dim);
    font-size: 11px;
  }
  .options summary {
    cursor: pointer;
    width: fit-content;
    list-style: none;
  }
  .options summary::-webkit-details-marker {
    display: none;
  }
  .options summary::before {
    content: "›";
    display: inline-block;
    margin-right: 7px;
    color: var(--dim);
  }
  .options[open] summary::before {
    transform: rotate(90deg);
  }
  .options summary span {
    opacity: 0.8;
    margin-left: 6px;
    font-size: 10px;
  }
  .opts {
    display: flex;
    flex-wrap: wrap;
    gap: 10px 24px;
    padding-top: 14px;
  }
  .opts label,
  .merge label {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .opts small {
    color: var(--dim);
  }
  .merge {
    margin-top: 16px;
    padding: 12px 14px;
    background: var(--panel-2);
    border: 1px solid var(--line);
    border-radius: 6px;
    font-size: 12px;
  }
  .merge small {
    display: block;
    margin: 4px 0 0 23px;
    color: var(--dim);
    font-size: 11px;
  }
  .form-footer {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 20px;
    margin-top: 20px;
  }
  .form-footer button {
    padding: 8px 18px;
  }
  .tip {
    color: var(--dim);
    font-size: 11px;
  }
  .form-error {
    color: var(--error);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    margin: 15px 0 0;
  }
  .mismatch {
    font-size: 12px;
    border: 1px solid var(--line);
    padding: 10px;
    border-radius: 6px;
  }
  .extra {
    display: grid;
    gap: 10px;
  }
  .extra.grid {
    grid-template-columns: repeat(3, minmax(0, 1fr));
  }
  .extra .wide {
    grid-column: 1 / -1;
  }
  .field > span {
    display: block;
    font-size: 11px;
    font-weight: 600;
    color: var(--dim);
    margin-bottom: 4px;
  }
  .note {
    color: var(--dim);
    font-size: 12px;
    margin: 10px 0 0;
  }
  .modes {
    display: flex;
    gap: 18px;
    margin-top: 14px;
    font-size: 12px;
  }
  .link {
    border: none;
    background: none;
    color: var(--accent);
    padding: 0;
  }
  .back {
    margin-bottom: 20px;
    font-size: 12px;
  }
  .pr-banner {
    background: var(--panel-2);
    border: 1px solid var(--line);
    border-radius: 7px;
    padding: 14px;
    margin-bottom: 16px;
  }
  .pr-banner strong,
  .pr-banner small {
    display: block;
  }
  .pr-banner small {
    color: var(--dim);
    font-size: 11px;
    margin-top: 5px;
  }
  .pr-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    font-size: 11px;
    color: var(--dim);
    margin-top: 10px;
  }
  .pr-meta span:first-child {
    color: var(--left-only);
  }
  .pr-pair {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 25px minmax(0, 1fr);
    gap: 10px;
    align-items: center;
    margin-top: 22px;
  }
  .pr-pair section {
    border: 1px solid var(--line);
    background: var(--panel-2);
    border-radius: 7px;
    padding: 15px;
  }
  .pr-pair small {
    display: block;
    color: var(--dim);
    font-size: 10px;
  }
  .pr-pair strong {
    display: block;
    font-size: 13px;
    margin: 6px 0;
  }
  .pr-pair code {
    display: block;
    font-size: 11px;
    color: var(--dim);
    overflow-wrap: anywhere;
  }
  .pr-pair section > span {
    display: block;
    font-size: 10px;
    margin-top: 12px;
    color: var(--accent);
  }
  .pr-pair .right > span {
    color: var(--left-only);
  }
  .pr-arrow {
    color: var(--dim);
    text-align: center;
  }
  .pr-help,
  .loading {
    font-size: 11px;
    color: var(--dim);
  }
  .recent {
    max-width: 1100px;
    margin: 20px auto 0;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    overflow: hidden;
  }
  .recent header {
    padding: 8px 14px;
    border-bottom: 1px solid var(--line);
    font-size: 11px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--dim);
  }
  .entry {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px 2px 4px;
    border-bottom: 1px solid var(--line);
  }
  .entry:last-child {
    border-bottom: none;
  }
  .entry:hover {
    background: var(--panel-2);
  }
  .open {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    border: none;
    background: none;
    text-align: left;
    padding: 5px 10px;
  }
  .names {
    font-weight: 600;
  }
  .paths {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: pre;
    font-size: 11px;
    color: var(--dim);
  }
  .when {
    flex: none;
    font-size: 11px;
    color: var(--dim);
    margin-right: 6px;
  }
  .icon {
    flex: none;
    border: none;
    background: none;
    color: var(--dim);
    padding: 2px 6px;
  }
  .icon:hover,
  .icon.pinned {
    color: var(--accent);
  }
  @media (max-width: 900px) {
    .wrap {
      padding: 20px 14px;
    }
    .dialog {
      padding: 20px;
    }
    .auto {
      display: none;
    }
    .sides {
      gap: 8px;
    }
  }
  @media (max-width: 760px) {
    .sides,
    .pr-pair {
      grid-template-columns: 1fr;
    }
    .swap {
      justify-self: center;
      transform: rotate(90deg);
    }
    .pr-arrow {
      transform: rotate(90deg);
    }
    .form-footer {
      align-items: flex-end;
    }
    .options summary span {
      display: block;
      margin: 5px 0 0 16px;
    }
  }
</style>
