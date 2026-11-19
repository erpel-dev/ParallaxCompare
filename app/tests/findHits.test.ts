import { test } from 'node:test';
import assert from 'node:assert/strict';
import { anchorFor, hitAtOrAfter } from '../src/lib/findHits.ts';
import type { Hit } from '../src/lib/api.ts';

// Two files with "foo" on rows 0 and 1 of both sides.
const hit = (row: number, side: 0 | 1, start = 0): Hit => ({ row, side, start, end: start + 3 });
const all = [hit(0, 0), hit(0, 1), hit(1, 0), hit(1, 1)];
const left = all.filter((h) => h.side === 0);
const right = all.filter((h) => h.side === 1);

test('changing the searched side stays on the row of the current match', () => {
  // Right → Left, on the first right match: the left match of the same row.
  assert.equal(hitAtOrAfter(left, anchorFor(right[0], 'left')), 0);
  // Both, with a right match current → Left.
  assert.equal(hitAtOrAfter(left, anchorFor(all[1], 'left')), 0);
  assert.equal(hitAtOrAfter(left, anchorFor(all[3], 'left')), 1);
  // Left → Right, and to Both, keep the row too.
  assert.equal(hitAtOrAfter(right, anchorFor(left[0], 'right')), 0);
  assert.equal(hitAtOrAfter(all, anchorFor(right[0], 'both')), 1);
});

test('a match earlier in the line on the other side is still found', () => {
  // Current: right, late in the line; the left match of that row starts earlier.
  const current = hit(4, 1, 20);
  assert.equal(hitAtOrAfter([hit(4, 0, 2), hit(9, 0)], anchorFor(current, 'left')), 0);
});

test('without a match on the row, the next row with one; none after it gives -1', () => {
  assert.equal(hitAtOrAfter([hit(0, 0), hit(5, 0)], anchorFor(hit(2, 1), 'left')), 1);
  assert.equal(hitAtOrAfter([hit(0, 0)], anchorFor(hit(2, 1), 'left')), -1);
  // On the same side the anchor is the match itself, with its offset.
  assert.deepEqual(anchorFor(hit(3, 0, 7), 'left'), hit(3, 0, 7));
  assert.equal(hitAtOrAfter([hit(3, 0, 1), hit(3, 0, 7)], anchorFor(hit(3, 0, 7), 'both')), 1);
});
