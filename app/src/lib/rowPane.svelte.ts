// Rows of a virtually scrolled pane, fetched from the backend in chunks as they
// come into view. Used by the merge view, whose two panes scroll separately.

import { scrollScale } from "./vscroll";

export const ROW = 19;
const CHUNK = 200;
/** Loaded chunks kept; those farthest from the view are dropped beyond this. */
const KEEP_CHUNKS = 40;

export class RowPane<T> {
  total = $state(0);
  scrollTop = $state(0);
  viewHeight = $state(400);
  /** Bumped when rows arrive or are dropped, to redraw. */
  version = $state(0);
  private cache = new Map<number, T>();
  private loaded = new Set<number>();
  private pending = new Set<number>();
  /** Changes when the rows are replaced, so older answers are dropped. */
  private generation = 0;

  constructor(private fetch: (start: number, count: number) => Promise<T[]>) {}

  get scale(): number {
    return scrollScale(this.total * ROW, this.viewHeight);
  }

  /** First and last (exclusive) row in view. */
  get window(): [number, number] {
    const first = Math.max(0, Math.floor((this.scrollTop * this.scale) / ROW));
    const last = Math.min(this.total, first + Math.ceil(this.viewHeight / ROW) + 2);
    return [first, last];
  }

  row(i: number): T | undefined {
    return this.cache.get(i);
  }

  /** New rows: everything cached is stale. */
  reset(total: number) {
    this.total = total;
    this.generation++;
    this.cache.clear();
    this.loaded.clear();
    this.pending.clear();
    this.version++;
    this.ensure();
  }

  /** Fetches the chunks in view and next to it. */
  ensure() {
    const [first, last] = this.window;
    const from = Math.floor(first / CHUNK);
    const to = Math.floor(Math.max(first, last - 1) / CHUNK);
    for (let c = Math.max(0, from - 1); c <= to + 1; c++) {
      if (c * CHUNK >= this.total || this.loaded.has(c) || this.pending.has(c)) continue;
      this.pending.add(c);
      const gen = this.generation;
      this.fetch(c * CHUNK, CHUNK)
        .then((rows) => {
          if (gen !== this.generation) return;
          rows.forEach((r, i) => this.cache.set(c * CHUNK + i, r));
          this.loaded.add(c);
          this.drop(from);
          this.version++;
        })
        .catch(() => {})
        .finally(() => {
          if (gen === this.generation) this.pending.delete(c);
        });
    }
  }

  private drop(near: number) {
    if (this.loaded.size <= KEEP_CHUNKS) return;
    const far = [...this.loaded].sort((a, b) => Math.abs(b - near) - Math.abs(a - near));
    for (const c of far.slice(0, this.loaded.size - KEEP_CHUNKS)) {
      this.loaded.delete(c);
      for (let i = c * CHUNK; i < (c + 1) * CHUNK; i++) this.cache.delete(i);
    }
  }
}
