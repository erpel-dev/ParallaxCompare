import type { RowKind, RowRun } from "./api";

export type RowFilter = "all" | "diffs" | RowKind;

/** A projection of row ranges. Keeps original row IDs without allocating an index per line. */
export class RowProjection {
  readonly runs: (RowRun & { offset: number })[] = [];
  readonly length: number;

  constructor(runs: RowRun[], filter: RowFilter) {
    let offset = 0;
    for (const run of runs) {
      if (filter !== "all" && (filter === "diffs" ? run.kind === "equal" || run.kind === "minor" : run.kind !== filter)) continue;
      this.runs.push({ ...run, offset });
      offset += run.end - run.start;
    }
    this.length = offset;
  }

  /** Display position to original row; null outside the projection. */
  source(at: number): number | null {
    if (at < 0 || at >= this.length) return null;
    let lo = 0, hi = this.runs.length;
    while (lo < hi) { const mid = (lo + hi) >>> 1; if (this.runs[mid].offset <= at) lo = mid + 1; else hi = mid; }
    const run = this.runs[lo - 1];
    return run.start + at - run.offset;
  }

  /** Original row to display position, or the insertion position if hidden. */
  position(row: number): number {
    let lo = 0, hi = this.runs.length;
    while (lo < hi) { const mid = (lo + hi) >>> 1; if (this.runs[mid].end <= row) lo = mid + 1; else hi = mid; }
    const run = this.runs[lo];
    return run ? run.offset + Math.max(0, row - run.start) : this.length;
  }

  contains(row: number): boolean { return this.source(this.position(row)) === row; }
}
