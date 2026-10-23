<script lang="ts" module>
  export { toSpec, type PickerValue } from "./sourcePicker";
</script>

<script lang="ts">
  import { tick } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import { api, isPullRequestUrl, sourceText, type RefList, type SourceSpec } from "./api";
  import { pickerFor, type PickerValue, type PickerPreview } from "./sourcePicker";
  import PathInput from "./PathInput.svelte";

  let {
    title,
    value = $bindable(),
    preview = $bindable(null),
    respectGitignore = true,
    onEnter,
    onPullRequest,
  }: {
    title: string;
    value: PickerValue;
    preview?: PickerPreview | null;
    respectGitignore?: boolean;
    onEnter?: () => void;
    onPullRequest?: (url: string) => void;
  } = $props();

  let retry = $state(0);
  let browseMenu = $state<HTMLDetailsElement>();
  let dialog = $state<HTMLDialogElement>();
  let revisionOpen = $state(false);
  let repository = $state("");
  let revision = $state("HEAD");
  let scope = $state("");
  let refs = $state<RefList | null>(null);
  let refsError = $state("");
  let refsLoading = $state(false);
  let filter = $state("");
  let revisionError = $state("");
  let saving = $state(false);
  let previewQueue: Promise<void> = Promise.resolve();

  // Each effect owns its response. Editing, swapping, retrying or unmounting
  // invalidates an in-flight request, including slow network-share metadata.
  $effect(() => {
    const input = value.path.trim(),
      explicit = value.spec;
    retry;
    preview = null;
    if (!input) return;
    if (isPullRequestUrl(input)) {
      preview = {
        input,
        kind: "remote",
        spec: null,
        gitRoot: null,
        gitSubpath: "",
        isFile: null,
        message: "Pull request: supplies both comparison sides.",
      };
      return;
    }
    let active = true;
    const timer = setTimeout(() => {
      // At most one filesystem inspection per card, even on a slow share.
      previewQueue = previewQueue.then(async () => {
        if (!active) return;
        try {
          const result = await api.previewSource(input, explicit ?? null);
          if (active) preview = { ...result, input };
        } catch (e) {
          if (active)
            preview = {
              input,
              kind: "error",
              message: String(e),
              spec: null,
              gitRoot: null,
              gitSubpath: "",
              isFile: null,
            };
        }
      });
    }, 350);
    return () => {
      active = false;
      clearTimeout(timer);
    };
  });

  function changed(path: string) {
    if (isPullRequestUrl(path) && onPullRequest) {
      onPullRequest(path.trim());
      return;
    }
    preview = null;
    value = { path };
  }

  const detected = $derived(preview?.input === value.path.trim() ? preview : null);
  const spec = $derived(value.spec ?? detected?.spec);
  const git = $derived(spec?.type === "gitRev" ? spec : null);
  const worktree = $derived(value.spec?.type === "gitWorktree");
  const label = $derived(
    !value.path.trim()
      ? "Not selected"
      : !detected
        ? "Checking…"
        : worktree
          ? "Working tree"
          : {
              folder: detected.gitRoot ? "Git folder" : "Folder",
              file: "File",
              archive: "Archive",
              archivePath: "Archive path",
              gitRev: "Git revision",
              remote: isPullRequestUrl(value.path) ? "Pull request" : "Repository link",
              needsRepository: "Repository needed",
              error: "Access / source error",
              empty: "Not selected",
            }[detected.kind],
  );
  const detail = $derived.by(() => {
    if (!value.path.trim()) return "Paste a path, repository link or PR URL";
    if (!detected) return "Checking source type and availability…";
    if (detected.message) return detected.message;
    switch (detected.kind) {
      case "folder":
        return worktree
          ? "Git working tree · ignored files hidden"
          : detected.gitRoot
            ? "Files on disk · Git repository detected"
            : "Folder on disk";
      case "file":
        return "Single file · compare its contents";
      case "archive":
        return "Archive · opened as a folder";
      case "archivePath":
        return "Inside an archive · contents checked when comparing";
      case "gitRev":
        return `${git?.rev ?? "Revision"} · saved revision`;
      case "remote":
        return "Repository link · access checked when comparing";
      default:
        return "Source is not available";
    }
  });
  const secondary = $derived.by(() => {
    if (git) return `${git.subpath ? `Scope: ${git.subpath}` : "Whole repository"} · read-only`;
    if (detected?.kind === "folder" && detected.gitRoot)
      return respectGitignore || worktree
        ? "Includes local changes; ignored files are hidden"
        : "Includes local changes and ignored files";
    if (detected?.kind === "remote") return "Uses your existing Git authentication";
    if (detected?.kind === "error") return "Correct the input or check your access, then retry.";
    if (detected?.kind === "needsRepository") return "Choose a repository to resolve a branch, tag or commit.";
    if (!value.path.trim()) return "Or use Browse to choose a file, archive or folder";
    return "Detected automatically";
  });

  async function browse(directory: boolean) {
    if (browseMenu) browseMenu.open = false;
    try {
      const picked = await open({
        directory,
        multiple: false,
        title: directory ? "Choose a folder" : "Choose a file or archive",
      });
      if (typeof picked === "string") changed(picked);
    } catch (e) {
      preview = {
        input: value.path.trim(),
        kind: "error",
        message: String(e),
        spec: null,
        gitRoot: null,
        gitSubpath: "",
        isFile: null,
      };
    }
  }

  async function chooseRevision() {
    repository = git?.repo ?? detected?.gitRoot ?? "";
    revision = git?.rev ?? (detected?.kind === "needsRepository" ? value.path.trim() : "HEAD");
    scope = git?.subpath ?? detected?.gitSubpath ?? "";
    refs = null;
    refsError = revisionError = filter = "";
    revisionOpen = true;
    await tick();
    dialog?.showModal();
  }

  // Ref enumeration is requested only after opening the revision chooser.
  $effect(() => {
    const repo = repository.trim();
    if (!revisionOpen || !repo) {
      refs = null;
      refsLoading = false;
      return;
    }
    let active = true;
    refs = null;
    refsError = "";
    refsLoading = true;
    const timer = setTimeout(async () => {
      try {
        const result = await api.gitRefs(repo);
        if (active) refs = result;
      } catch (e) {
        if (active) refsError = String(e);
      } finally {
        if (active) refsLoading = false;
      }
    }, 300);
    return () => {
      active = false;
      clearTimeout(timer);
    };
  });

  async function saveRevision() {
    if (saving) return;
    const chosen: SourceSpec = {
      type: "gitRev",
      repo: repository.trim(),
      rev: revision.trim() || "HEAD",
      subpath: scope.trim(),
    };
    if (!chosen.repo) {
      revisionError = "Choose the repository containing this revision.";
      return;
    }
    saving = true;
    revisionError = "";
    try {
      const result = await api.previewSource(sourceText(chosen), chosen);
      if (result.kind === "error" || result.kind === "needsRepository") {
        revisionError = result.message ?? "Cannot resolve this revision.";
        return;
      }
      value = pickerFor(chosen);
      preview = { ...result, input: value.path.trim() };
      dialog?.close();
    } catch (e) {
      revisionError = String(e);
    } finally {
      saving = false;
    }
  }

  function useDisk() {
    if (!git) return;
    const local = git.repo + (git.subpath ? `/${git.subpath}` : "");
    changed(local);
  }

  const matches = (text: string) => !filter || text.toLowerCase().includes(filter.toLowerCase());
</script>

<svelte:window
  onmousedown={(e) => {
    if (browseMenu?.open && !browseMenu.contains(e.target as Node)) browseMenu.open = false;
  }}
  onkeydown={(e) => {
    if (e.key === "Escape" && browseMenu?.open) {
      e.stopPropagation();
      browseMenu.open = false;
      browseMenu.querySelector("summary")?.focus();
    }
  }}
/>

<section class="picker" class:right={title.toLowerCase().startsWith("right")} aria-label="{title} source">
  <header>
    <h4>{title} source</h4>
    <span
      class="type"
      class:git={!!git || detected?.kind === "remote"}
      class:bad={detected?.kind === "error"}
      class:pending={detected?.kind === "needsRepository"}>{label}</span
    >
  </header>
  <div class="body">
    <div class="row">
      <PathInput
        bind:value={() => value.path, changed}
        label="{title} path"
        placeholder="Paste a path or repository link"
        {onEnter}
      />
      <details class="browse" bind:this={browseMenu}>
        <summary aria-label="Browse for {title.toLowerCase()} source">Browse ⌄</summary>
        <div class="browse-menu">
          <button type="button" onclick={() => browse(true)}>Choose folder…</button>
          <button type="button" onclick={() => browse(false)}>Choose file or archive…</button>
        </div>
      </details>
    </div>
    <div class="detected" role="status" aria-live="polite">
      <span
        class="status-icon"
        class:bad={detected?.kind === "error"}
        class:pending={detected?.kind === "needsRepository"}
        >{!value.path.trim()
          ? "↗"
          : !detected
            ? "…"
            : detected.kind === "error"
              ? "!"
              : detected.kind === "needsRepository"
                ? "?"
                : "✓"}</span
      >
      <div><strong>{detail}</strong><small>{secondary}</small></div>
    </div>
    <div class="context">
      {#if git || detected?.gitRoot || detected?.kind === "needsRepository"}
        <span>{git ? "Read-only revision" : detected?.gitRoot ? "Git repository" : "Revision needs context"}</span>
        <div class="actions">
          {#if git && detected?.kind !== "remote"}<button type="button" class="link" onclick={useDisk}
              >Use files on disk</button
            >{/if}
          <button type="button" class="link" onclick={chooseRevision}
            >{git
              ? "Change revision…"
              : detected?.kind === "needsRepository"
                ? "Choose repository…"
                : "Choose revision…"}</button
          >
        </div>
      {:else if detected?.kind === "error"}
        <button type="button" class="link" onclick={() => retry++}>Retry detection</button><span
          >Input kept for correction</span
        >
      {:else}
        <span
          >{detected?.kind === "archive"
            ? "No extraction step needed"
            : detected?.kind === "remote"
              ? "Read-only · resolved on Compare"
              : "Paths and repository links work here"}</span
        >
        <span class="complete"><kbd>Tab</kbd> complete</span>
      {/if}
    </div>
  </div>
</section>

<dialog
  bind:this={dialog}
  onclose={() => (revisionOpen = false)}
  oncancel={(e) => {
    if (saving) e.preventDefault();
  }}
  onkeydown={(e) => {
    if (e.key === "Escape") e.stopPropagation();
    if (e.key === "Enter" && (e.target as HTMLElement).tagName === "INPUT") {
      e.preventDefault();
      e.stopPropagation();
      saveRevision();
    }
  }}
>
  <div class="revision-dialog">
    <header>
      <h3>Choose a Git revision</h3>
      <button
        type="button"
        disabled={saving}
        class="close"
        aria-label="Close revision chooser"
        onclick={() => dialog?.close()}>×</button
      >
    </header>
    <p>Choose a branch, tag or commit. The revision is read-only.</p>
    <fieldset disabled={saving}>
      <div class="field">Repository</div>
      <div class="row">
        <PathInput
          bind:value={repository}
          label="Revision repository"
          placeholder="Repository folder or clone URL"
        /><button
          type="button"
          onclick={async () => {
            try {
              const p = await open({ directory: true, multiple: false, title: "Choose a Git repository" });
              if (typeof p === "string") repository = p;
            } catch (e) {
              revisionError = String(e);
            }
          }}>Browse…</button
        >
      </div>
      <label class="field"
        >Revision<input
          type="text"
          class="mono"
          aria-label="Revision"
          bind:value={revision}
          placeholder="HEAD, branch, tag or commit"
          spellcheck="false"
        /></label
      >
      <label class="field"
        >File or subfolder <span class="dim">· optional</span><input
          type="text"
          class="mono"
          aria-label="Revision scope"
          bind:value={scope}
          placeholder="e.g. docs or src/main.rs"
          spellcheck="false"
        /></label
      >
      {#if refsLoading}<p class="dim" role="status">Reading refs…</p>{/if}
      {#if refsError}<p class="error" role="alert">{refsError}</p>{/if}
      {#if refs}
        <input
          type="text"
          class="filter"
          aria-label="Filter revisions"
          placeholder="Filter branches, tags and commits…"
          bind:value={filter}
        />
        <div class="reflist">
          {#each [["Branches", refs.branches], ["Remote branches", refs.remotes], ["Tags", refs.tags]] as const as [name, list] (name)}
            {@const shown = list.filter((r) => matches(r.name))}
            {#if shown.length}<div class="grp">{name}</div>
              {#each shown as r (r.name)}<button
                  type="button"
                  class="ref"
                  class:on={revision === r.name}
                  onclick={() => (revision = r.name)}
                  ><span>{r.name}</span><span class="mono dim">{r.shortId}</span></button
                >{/each}{/if}
          {/each}
          {#if refs.recentCommits.some((c) => matches(c.summary) || matches(c.shortId))}
            <div class="grp">Recent commits</div>
            {#each refs.recentCommits.filter((c) => matches(c.summary) || matches(c.shortId)) as c (c.id)}<button
                type="button"
                class="ref"
                class:on={revision === c.id}
                onclick={() => (revision = c.id)}
                ><span class="ellipsis">{c.summary}</span><span class="mono dim">{c.shortId}</span></button
              >{/each}
          {/if}
        </div>
      {/if}
      {#if revisionError}<p class="error" role="alert">{revisionError}</p>{/if}
      <footer>
        <button type="button" onclick={() => dialog?.close()}>Cancel</button><button
          type="button"
          class="primary"
          onclick={saveRevision}>{saving ? "Checking…" : "Use revision"}</button
        >
      </footer>
    </fieldset>
  </div>
</dialog>

<style>
  .picker {
    min-width: 0;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--input);
  }
  .picker > header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 8px;
    padding: 11px 14px;
    border-bottom: 1px solid var(--line);
    background: var(--panel-2);
    border-radius: 8px 8px 0 0;
  }
  h4 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    display: flex;
    align-items: center;
    gap: 7px;
  }
  h4::before {
    content: "";
    width: 6px;
    height: 6px;
    border-radius: 2px;
    background: var(--accent);
  }
  .right h4::before {
    background: var(--left-only);
  }
  .type {
    font-size: 10px;
    padding: 2px 7px;
    border-radius: 4px;
    color: var(--dim);
    background: var(--panel);
  }
  .type.git {
    color: var(--left-only);
  }
  .type.bad,
  .error,
  .status-icon.bad {
    color: var(--error);
  }
  .type.pending,
  .status-icon.pending {
    color: var(--similar);
  }
  .body {
    padding: 14px;
  }
  .row {
    display: flex;
    align-items: stretch;
    gap: 6px;
    min-width: 0;
  }
  .row :global(input) {
    padding: 9px 10px;
    font-size: 12px;
  }
  .browse {
    position: relative;
  }
  .browse summary {
    list-style: none;
    cursor: pointer;
    white-space: nowrap;
    border: 1px solid var(--btn-border);
    border-radius: 5px;
    padding: 9px 10px;
    font-size: 12px;
    background: var(--btn);
  }
  .browse summary::-webkit-details-marker {
    display: none;
  }
  .browse-menu {
    position: absolute;
    top: calc(100% + 4px);
    right: 0;
    z-index: 50;
    background: var(--panel);
    border: 1px solid var(--line);
    padding: 5px;
    border-radius: 6px;
    box-shadow: 0 6px 20px #0003;
    min-width: 190px;
  }
  .browse-menu button {
    display: block;
    width: 100%;
    text-align: left;
    background: none;
    border: 0;
    padding: 8px;
  }
  .browse-menu button:hover {
    background: var(--sel);
  }
  .detected {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    min-height: 65px;
    padding-top: 13px;
  }
  .status-icon {
    flex: none;
    width: 18px;
    height: 18px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    background: var(--panel);
    color: var(--right-only);
    font-size: 11px;
  }
  .detected strong {
    display: block;
    font-size: 12px;
    font-weight: 550;
    overflow-wrap: anywhere;
  }
  .detected small {
    display: block;
    color: var(--dim);
    font-size: 11px;
    margin: 3px 0 10px;
    overflow-wrap: anywhere;
  }
  .context {
    display: flex;
    gap: 8px;
    justify-content: space-between;
    align-items: center;
    flex-wrap: wrap;
    border-top: 1px solid var(--line);
    padding-top: 10px;
    color: var(--dim);
    font-size: 10px;
  }
  .actions {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
    margin-left: auto;
  }
  .link {
    background: none;
    border: none;
    color: var(--accent);
    padding: 0;
    font-size: 11px;
  }
  .link:hover {
    text-decoration: underline;
  }
  .complete {
    display: flex;
    align-items: center;
    gap: 5px;
  }
  kbd {
    border: 1px solid var(--line);
    padding: 0 4px;
    border-radius: 3px;
    font-size: 9px;
  }
  dialog {
    width: 570px;
    max-width: calc(100vw - 40px);
    max-height: calc(100vh - 40px);
    padding: 0;
    color: var(--text);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 10px;
    box-shadow: 0 20px 90px #0007;
  }
  dialog::backdrop {
    background: #14243c88;
  }
  .revision-dialog {
    padding: 22px;
  }
  .revision-dialog header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  h3 {
    font-size: 19px;
    margin: 0;
  }
  .revision-dialog > p {
    color: var(--dim);
    font-size: 12px;
    margin-bottom: 19px;
  }
  fieldset {
    border: 0;
    padding: 0;
    margin: 0;
    min-width: 0;
  }
  .field {
    display: block;
    margin: 13px 0 6px;
    font-size: 12px;
  }
  .field input {
    display: block;
    margin-top: 6px;
    width: 100%;
    padding: 8px 10px;
  }
  .dim {
    color: var(--dim);
    font-size: 11px;
  }
  .close {
    border: 0;
    background: var(--panel-2);
    font-size: 20px;
  }
  .error {
    overflow-wrap: anywhere;
    white-space: pre-wrap;
    font-size: 12px;
  }
  .filter {
    margin-top: 15px;
    width: 100%;
  }
  .reflist {
    max-height: 230px;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: 5px;
    margin-top: 7px;
  }
  .grp {
    position: sticky;
    top: 0;
    padding: 4px 9px;
    font-size: 10px;
    text-transform: uppercase;
    color: var(--dim);
    background: var(--panel-2);
  }
  .ref {
    display: flex;
    width: 100%;
    justify-content: space-between;
    gap: 12px;
    border: none;
    border-radius: 0;
    background: none;
    padding: 5px 9px;
    text-align: left;
  }
  .ref:hover {
    background: var(--sel);
  }
  .ellipsis {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 20px;
  }
  footer button {
    padding: 7px 14px;
  }
  summary:focus-visible,
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
