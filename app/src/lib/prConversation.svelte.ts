/**
 * The conversation of the pull request a comparison shows, per comparison
 * session: read once in the background when the comparison opens, kept for
 * the folder view and every file opened from it, read again on request.
 */
import { api, type PrAnnotation, type PrConversation, type PrThread, type PullRequestInfo } from "./api";

export interface PrState {
  info: PullRequestInfo;
  /** The head commit the comparison shows. */
  head: string;
  loading: boolean;
  data: PrConversation | null;
  error: string | null;
  at: number | null;
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
  /** A thread a file tab should show once it has opened: session → path → index in `threads`. */
  focus = $state<{ session: number; path: string; thread: number } | null>(null);

  register(session: number, info: PullRequestInfo, head: string) {
    if (this.bySession[session]?.head === head) return;
    this.bySession[session] = { info, head, loading: false, data: null, error: null, at: null };
    void this.load(session);
  }

  get(session: number): PrState | undefined {
    return this.bySession[session];
  }

  async load(session: number) {
    const s = this.bySession[session];
    if (!s || s.loading) return;
    s.loading = true;
    s.error = null;
    try {
      const data = await api.pullRequestConversation(s.info.url, s.head);
      const now = this.bySession[session];
      if (now && now.head === s.head) {
        now.data = data;
        now.at = Date.now();
      }
    } catch (e) {
      const now = this.bySession[session];
      if (now) now.error = String(e);
    } finally {
      const now = this.bySession[session];
      if (now) now.loading = false;
    }
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
