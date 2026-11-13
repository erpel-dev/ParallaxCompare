/**
 * The conversation of the pull request a comparison shows, per comparison
 * session: read once in the background when the comparison opens, kept for
 * the folder view and every file opened from it, read again on request.
 */
import { api, type PrAnnotation, type PrConversation, type PrIntoReview, type PrThread, type PrWrite, type PullRequestInfo } from "./api";
import { notify } from "./toast.svelte";

export interface PrState {
  info: PullRequestInfo;
  /** The head commit the comparison shows. */
  head: string;
  loading: boolean;
  checksLoading: boolean;
  checksError: string | null;
  checksTruncated: boolean;
  annotationsTruncated: boolean;
  filesLoading: boolean;
  filesError: string | null;
  filesTruncated: boolean;
  generation: number;
  data: PrConversation | null;
  error: string | null;
  at: number | null;
}

/** A new comment being written: on lines `start..end` (1-based) of a side, or on the file (`end` 0). */
export interface ComposerTarget {
  session: number;
  path: string;
  side: "left" | "right";
  start: number;
  end: number;
  /** The selected lines of the head, offered as a suggested change. */
  suggestion?: string[] | null;
}

/** Open threads, findings and CI annotations of one file or folder. */
export interface PrCounts {
  open: number;
  findings: number;
  warnings: number;
  resolved: number;
}

class PrConversations {
  bySession = $state<Record<number, PrState>>({});
  /** What a file tab should show once it has opened: a thread (index in `threads`), or a line of the head. */
  focus = $state<{ session: number; path: string; thread?: number; line?: number } | null>(null);
  /** The new comment the thread panel shows a composer for. */
  composer = $state<ComposerTarget | null>(null);
  /** The thread (index) whose reply field should take the focus. */
  replyTo = $state<{ session: number; thread: number } | null>(null);
  /** The session whose Finish review dialog is open. */
  reviewing = $state<number | null>(null);
  /** A write is on its way, per session. */
  busy = $state<Record<number, boolean>>({});

  /**
   * Makes one change on GitHub, then reads the conversation again. Returns
   * whether it was made; GitHub's message is shown when not.
   */
  async write(session: number, change: PrWrite): Promise<boolean> {
    const s = this.bySession[session];
    if (!s || this.busy[session]) return false;
    if (s.loading || s.error) { notify("Refresh the comments successfully before sending a change"); return false; }
    this.busy[session] = true;
    try {
      await api.pullRequestWrite(s.info.url, change);
      return true;
    } catch (e) {
      notify(String(e));
      return false;
    } finally {
      if (this.bySession[session] === s) { this.busy[session] = false; void this.load(session); }
    }
  }

  register(session: number, info: PullRequestInfo, head: string) {
    if (this.bySession[session]?.head === head) return;
    this.bySession[session] = { info, head, loading: false, data: null, error: null, at: null,
      checksLoading: false, checksError: null, checksTruncated: false, annotationsTruncated: false,
      filesLoading: false, filesError: null, filesTruncated: false, generation: 0 };
    void this.load(session);
  }

  get(session: number): PrState | undefined {
    return this.bySession[session];
  }

  /** Release cached data and invalidate pending reads when a comparison closes. */
  release(session: number) {
    delete this.bySession[session]; delete this.busy[session];
    if (this.focus?.session === session) this.focus = null;
    if (this.composer?.session === session) this.composer = null;
    if (this.replyTo?.session === session) this.replyTo = null;
    if (this.reviewing === session) this.reviewing = null;
  }
  async load(session: number) {
    const s = this.bySession[session]; if (!s) return;
    const generation = ++s.generation;
    const current = () => this.bySession[session] === s && s.generation === generation;
    s.loading = s.checksLoading = s.filesLoading = true;
    s.error = s.checksError = s.filesError = null;
    if (s.data) s.data.files = {};
    s.filesTruncated = false;
    let checks: Awaited<ReturnType<typeof api.pullRequestChecks>> | null = null;
    let files: Awaited<ReturnType<typeof api.pullRequestFiles>> | null = null;
    const applyChecks = () => {
      if (!s.data || !checks) return;
      s.data.checks = checks.checks; s.data.annotations = checks.annotations;
      s.checksTruncated = checks.checksTruncated; s.annotationsTruncated = checks.annotationsTruncated;
    };
    const applyFiles = () => {
      if (!s.data || !files || s.loading || !linesApply(s.data)) return;
      s.data.files = files.files; s.filesTruncated = files.truncated;
    };
    // Each part is delivered as soon as it finishes; optional reads cannot delay comments/errors.
    await Promise.allSettled([
      api.pullRequestConversation(s.info.url, s.head).then((data) => {
        if (!current()) return;
        const cached = s.data;
        s.data = { ...data, checks: cached?.checks ?? data.checks, annotations: cached?.annotations ?? data.annotations };
        s.loading = false; s.at = Date.now(); applyChecks(); applyFiles();
      }).catch((error) => { if (current()) s.error = String(error); }).finally(() => { if (current()) s.loading = false; }),
      api.pullRequestChecks(s.info.url, s.head).then((data) => { if (current()) { checks = data; applyChecks(); } })
        .catch((error) => { if (current()) s.checksError = String(error); }).finally(() => { if (current()) s.checksLoading = false; }),
      api.pullRequestFiles(s.info.url).then((data) => { if (current()) { files = data; applyFiles(); } })
        .catch((error) => { if (current()) s.filesError = String(error); }).finally(() => { if (current()) s.filesLoading = false; }),
    ]);
  }

}

export const prConversations = new PrConversations();

/** Whether the comparison's head is the one the threads' lines refer to. */
export const linesApply = (c: PrConversation) => c.head === c.pinnedHead;

/** Counts per file path, and per folder (every prefix), for the folder view. */
export function countsByPath(c: PrConversation): Map<string, PrCounts> {
  const map = new Map<string, PrCounts>();
  const add = (path: string, f: (n: PrCounts) => void) => {
    const parts = path.split("/");
    for (let i = parts.length; i >= 1; i--) {
      const key = parts.slice(0, i).join("/");
      let n = map.get(key);
      if (!n) map.set(key, (n = { open: 0, findings: 0, warnings: 0, resolved: 0 }));
      f(n);
    }
  };
  for (const t of c.threads) {
    if (t.resolved) add(t.path, (n) => n.resolved++);
    else if (t.severity === "Important") add(t.path, (n) => n.findings++);
    else add(t.path, (n) => n.open++);
  }
  for (const a of c.annotations) add(a.path, (n) => (a.level === "failure" ? n.findings++ : n.warnings++));
  return map;
}

/** The latest review state per reviewer, newest first. */
export function reviewSummary(c: PrConversation): { author: string; state: string; bot: boolean }[] {
  const latest = new Map<string, { author: string; state: string; bot: boolean; at: string }>();
  for (const r of c.reviews) {
    const author = r.author ?? "ghost";
    // A comment after an approval does not take the approval back.
    if (r.state === "COMMENTED" && latest.has(author)) continue;
    latest.set(author, { author, state: r.state, bot: r.bot, at: r.submitted ?? "" });
  }
  return [...latest.values()].sort((a, b) => b.at.localeCompare(a.at));
}

/** Markers of one file: threads and annotations by side and 0-based line. */
export interface FileMarks {
  threads: { index: number; thread: PrThread; side: "left" | "right"; from: number; to: number }[];
  annotations: (PrAnnotation & { from: number; to: number })[];
  /** Threads that have no line in this comparison: outdated, on the file, or made on a newer head. */
  unplaced: { index: number; thread: PrThread }[];
}

export function marksFor(c: PrConversation, path: string): FileMarks {
  const marks: FileMarks = { threads: [], annotations: [], unplaced: [] };
  const apply = linesApply(c);
  c.threads.forEach((thread, index) => {
    if (thread.path !== path) return;
    if (!apply || thread.outdated || thread.fileLevel || thread.line === null) {
      marks.unplaced.push({ index, thread });
      return;
    }
    const to = thread.line - 1;
    const from = (thread.startLine ?? thread.line) - 1;
    marks.threads.push({ index, thread, side: thread.side, from: Math.min(from, to), to });
  });
  // Annotations belong to the compared head itself, so their lines always apply.
  for (const a of c.annotations) if (a.path === path) marks.annotations.push({ ...a, from: a.startLine - 1, to: a.endLine - 1 });
  marks.threads.sort((a, b) => a.to - b.to);
  return marks;
}

/** "3 h ago", "yesterday", or the date. */
export function ago(iso: string | null): string {
  if (!iso) return "";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "";
  const s = (Date.now() - t) / 1000;
  if (s < 90) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  if (s < 172800) return "yesterday";
  if (s < 30 * 86400) return `${Math.round(s / 86400)} days ago`;
  return new Date(t).toLocaleDateString();
}

/** Where a new comment goes when it is added to the review: the pending one, or a new one on the compared head. */
export const intoReview = (c: PrConversation): PrIntoReview => ({ id: c.review?.id ?? null, pull: c.id, head: c.pinnedHead });

/** Why new comments on lines cannot be written here, or null if they can. */
export function linesBlocked(c: PrConversation): string | null {
  if (!c.viewer) return "Sign in with gh to comment";
  if (!c.canComment) return "You can read this pull request but not comment on it";
  if (!linesApply(c)) return "The pull request has a newer head: open it to comment on lines";
  if (c.review?.commit && c.review.commit !== c.pinnedHead) return "Your pending review is on another commit: finish or discard it first";
  return null;
}

/**
 * Whether GitHub takes a comment on lines `from..to` (1-based) of a file's side:
 * "lines" inside one hunk of its diff, "file" for a changed file otherwise, or
 * null for a file the pull request does not change.
 */
export function commentable(c: PrConversation, path: string, side: "left" | "right", from: number, to: number): "lines" | "file" | null {
  const f = c.files[path];
  if (!f) return null;
  return f[side].some(([a, b]) => from >= a && to <= b) ? "lines" : "file";
}

/** Whether one line (1-based) is inside a hunk of the file's diff on that side. */
export function lineCommentable(c: PrConversation, path: string, side: "left" | "right", line: number): boolean {
  return c.files[path]?.[side].some(([a, b]) => line >= a && line <= b) ?? false;
}

/** Text being written, kept on this computer until it is sent, so a crash does not lose it. */
export const unsent = {
  key: (url: string, target: string) => `parallax.prText:${url}:${target}`,
  get(url: string, target: string): string {
    try {
      return localStorage.getItem(this.key(url, target)) ?? "";
    } catch {
      return "";
    }
  },
  set(url: string, target: string, text: string) {
    try {
      if (text.trim()) localStorage.setItem(this.key(url, target), text);
      else localStorage.removeItem(this.key(url, target));
    } catch {
      // Not kept: the text stays in the field.
    }
  },
};
