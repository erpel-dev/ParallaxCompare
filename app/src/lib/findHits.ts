// Where the find bar goes after a search: the match at or after an anchor.
import type { Hit, SearchOptions } from "./api";

/** A place in the comparison: a row, a side (0 left, 1 right) and an offset in the line. */
export interface Anchor {
  row: number;
  side: 0 | 1;
  start: number;
}

/**
 * The anchor for a search of `side` that should stay near `current`. A match on
 * a side that is no longer searched cannot be the anchor itself: the search
 * then starts at the beginning of the same row on the side searched, so a
 * match there is found before any on a later row.
 */
export function anchorFor(current: Anchor, side: SearchOptions["side"]): Anchor {
  const searched = side === "left" ? 0 : side === "right" ? 1 : current.side;
  return searched === current.side ? current : { row: current.row, side: searched, start: 0 };
}

/** The index of the first match at or after `from`, in row order, left before right; -1 if none. */
export function hitAtOrAfter(hits: Hit[], from: Anchor): number {
  return hits.findIndex(
    (h) => h.row > from.row || (h.row === from.row && (h.side > from.side || (h.side === from.side && h.start >= from.start))),
  );
}
