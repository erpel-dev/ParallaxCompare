// Recently compared paths and comparisons, newest first, kept in this browser profile.

import type { PullRequestInfo, SourceOptions, SourceSpec } from "./api";

const KEY = "parallax.recentPaths";
const MAX = 30;

export function recentPaths(): string[] {
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(saved) ? saved.filter((p) => typeof p === "string") : [];
  } catch {
    return [];
  }
}

export function rememberPaths(...paths: string[]) {
  const fresh = paths.map((p) => p.trim()).filter(Boolean);
  if (!fresh.length) return;
  const list = [...fresh, ...recentPaths().filter((p) => !fresh.includes(p))].slice(0, MAX);
  try {
    localStorage.setItem(KEY, JSON.stringify(list));
  } catch {
    // Remembering paths is a convenience only.
  }
}

/** A comparison as it was started: both sides and the options. */
export interface SavedComparison {
  left: SourceSpec;
  right: SourceSpec;
  options: SourceOptions;
  mergeBase: boolean;
  pullRequest?: PullRequestInfo;
  /** Pinned comparisons stay at the top and are never pushed out. */
  pinned?: boolean;
  /** When it was last opened, milliseconds since the epoch. */
  used: number;
}

const COMPARISONS = "parallax.recentComparisons";
const MAX_UNPINNED = 12;

export const comparisonKey = (c: Pick<SavedComparison, "left" | "right" | "mergeBase">) =>
  JSON.stringify([c.left, c.right, c.mergeBase]);

export function recentComparisons(): SavedComparison[] {
  try {
    const saved = JSON.parse(localStorage.getItem(COMPARISONS) ?? "[]");
    return Array.isArray(saved) ? saved.filter((c) => c && c.left && c.right) : [];
  } catch {
    return [];
  }
}

function store(list: SavedComparison[]) {
  const pinned = list.filter((c) => c.pinned);
  const rest = list.filter((c) => !c.pinned).slice(0, MAX_UNPINNED);
  try {
    localStorage.setItem(COMPARISONS, JSON.stringify([...pinned, ...rest]));
  } catch {
    // Remembering comparisons is a convenience only.
  }
}

export function rememberComparison(c: Omit<SavedComparison, "used" | "pinned">) {
  const list = recentComparisons();
  const key = comparisonKey(c);
  const old = list.find((x) => comparisonKey(x) === key);
  const entry: SavedComparison = { ...c, pinned: old?.pinned, used: Date.now() };
  store([entry, ...list.filter((x) => comparisonKey(x) !== key)]);
}

export function pinComparison(key: string, pinned: boolean) {
  store(recentComparisons().map((c) => (comparisonKey(c) === key ? { ...c, pinned } : c)));
}

export function forgetComparison(key: string) {
  store(recentComparisons().filter((c) => comparisonKey(c) !== key));
}
