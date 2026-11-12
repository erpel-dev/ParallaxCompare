import { test } from 'node:test';
import assert from 'node:assert/strict';
import { selectionTargets } from '../src/lib/selection.ts';

test('prunes ancestors regardless of display order, preserving unrelated paths', () => {
  assert.deepEqual(selectionTargets(['a/b/c', 'a-b', 'a/b', 'z/child', 'a', 'z']), ['a-b', 'a', 'z']);
  assert.deepEqual(selectionTargets(['folder/file', 'folder2/file']), ['folder/file', 'folder2/file']);
  assert.deepEqual(selectionTargets(['leaf', '', 'other']), ['']);
  assert.deepEqual(selectionTargets([]), []);
});

test('bulk sibling selection and mixed selected folders remain practical', () => {
  const siblings = Array.from({ length: 100000 }, (_, i) => `file-${i}.txt`);
  assert.deepEqual(selectionTargets(siblings), siblings);
  const mixed = siblings.flatMap((p) => [p, `${p}/child`, `${p}/child/leaf`]);
  assert.deepEqual(selectionTargets(mixed), siblings);
});
