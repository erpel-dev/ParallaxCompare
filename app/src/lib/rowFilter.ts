import type { RowKind, RowRun } from "./api";

export type RowFilter = "all" | "diffs" | RowKind;

/** A stretch of hidden rows, shown as one bar. */
export interface Fold { start: number; end: number }

interface Part { start: number; end: number; offset: number; fold: boolean }

/**
 * A projection of row ranges. Keeps original row IDs without allocating an index
 * per line. With a filter, the matching rows keep `context` rows around them, and
 * every stretch of hidden rows takes one display position: a fold bar. `opened`
 * stretches are shown although the filter hides them.
 */
export class RowProjection {
  readonly parts: Part[] = [];
  readonly length: number;
  /** Display positions that are fold bars. */
  readonly folds: number;

  constructor(runs: RowRun[], filter: RowFilter, context = 0, opened: Fold[] = []) {
    const total = runs.length ? runs[runs.length - 1].end : 0;
    // The shown stretches: matching runs with their context, and what was opened.
    const shown: [number, number][] = [];
    for (const run of runs) {
      if (filter !== "all" && (filter === "diffs" ? run.kind === "equal" || run.kind === "minor" : run.kind !== filter)) continue;
      const pad = filter === "all" ? 0 : context;
      shown.push([Math.max(0, run.start - pad), Math.min(total, run.end + pad)]);
    }
    if (filter !== "all") for (const o of opened) shown.push([Math.max(0, o.start), Math.min(total, o.end)]);
    shown.sort((a, b) => a[0] - b[0]);
    let offset = 0, at = 0, folds = 0;
    const fold = (start: number, end: number) => {
      if (end <= start || filter === "all") return;
      this.parts.push({ start, end, offset, fold: true });
      offset += 1;
      folds += 1;
    };
    for (const [start, end] of shown) {
      if (end <= at) continue;
      const from = Math.max(start, at);
      const last = this.parts[this.parts.length - 1];
      if (last && !last.fold && last.end === from) last.end = end;
      else {
        fold(at, from);
        this.parts.push({ start: from, end, offset, fold: false });
      }
      offset += end - from;
      at = end;
    }
    // Nothing matches: an empty view, not one bar over the whole file.
    if (this.parts.length) fold(at, total);
    this.length = offset;
    this.folds = folds;
  }

  private at(pos: number): Part | null {
    if (pos < 0 || pos >= this.length) return null;
    let lo = 0, hi = this.parts.length;
    while (lo < hi) { const mid = (lo + hi) >>> 1; if (this.parts[mid].offset <= pos) lo = mid + 1; else hi = mid; }
    return this.parts[lo - 1];
  }

  /** Display position to original row; null outside the projection and on a fold bar. */
  source(at: number): number | null {
    const part = this.at(at);
    return part && !part.fold ? part.start + at - part.offset : null;
  }

  /** The hidden rows behind the fold bar at a display position. */
  fold(at: number): Fold | null {
    const part = this.at(at);
    return part?.fold ? { start: part.start, end: part.end } : null;
  }

  /** Original row to display position; the position of its fold bar if hidden. */
  position(row: number): number {
    let lo = 0, hi = this.parts.length;
    while (lo < hi) { const mid = (lo + hi) >>> 1; if (this.parts[mid].end <= row) lo = mid + 1; else hi = mid; }
    const part = this.parts[lo];
    return part ? part.offset + (part.fold ? 0 : Math.max(0, row - part.start)) : this.length;
  }

  contains(row: number): boolean { return this.source(this.position(row)) === row; }

  /** The row at display position `at`, or the nearest one in direction `dir` past fold bars. */
  rowNear(at: number, dir: 1 | -1): number | null {
    at = Math.max(0, Math.min(this.length - 1, at));
    for (let i = at; i >= 0 && i < this.length; i += dir) {
      const row = this.source(i);
      if (row !== null) return row;
    }
    for (let i = at; i >= 0 && i < this.length; i -= dir) {
      const row = this.source(i);
      if (row !== null) return row;
    }
    return null;
  }
}
