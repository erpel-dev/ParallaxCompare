import { test } from 'node:test';
import assert from 'node:assert/strict';
import { countFiles, flatten, matchingFolders, refreshTarget } from '../src/lib/tree.ts';
import type { Node, Side } from '../src/lib/api.ts';

const dir: Side = { kind: 'dir', size: 0, mtime: null };
const file: Side = { kind: 'file', size: 1, mtime: null };

// What differs is loaded; the identical folder came without its entries.
const root: Node = {
  name: '', left: dir, right: dir, status: 'different',
  children: [
    { name: 'equal', left: dir, right: dir, status: 'same', unexpanded: true, sameFiles: 1200 },
    { name: 'mixed', left: dir, right: dir, status: 'different', children: [
      { name: 'quiet', left: dir, right: dir, status: 'same', unexpanded: true, sameFiles: 3 },
      { name: 'b.txt', left: file, right: file, status: 'different' },
    ] },
    { name: 'top.txt', left: file, right: file, status: 'same' },
  ],
};

test('identical folders without their entries still count their files', () => {
  const c = countFiles(root);
  assert.equal(c.files, 1205);
  assert.equal(c.same, 1204);
  assert.equal(c.different, 1);
  // Collapsed, such a folder shows nothing that is not the same.
  assert.deepEqual(countFiles(root.children![0]), { files: 0, same: 0, similar: 0, different: 0, leftOnly: 0, rightOnly: 0, errors: 0 });
});

test('filters show them only where identical files are shown', () => {
  const names = (filter: Parameters<typeof flatten>[2]) => flatten(root, new Set(['mixed']), filter).map((r) => r.path);
  assert.deepEqual(names('all'), ['equal', 'mixed', 'mixed/quiet', 'mixed/b.txt', 'top.txt']);
  assert.deepEqual(names('diffs'), ['mixed', 'mixed/b.txt']);
  assert.deepEqual(names('same'), ['equal', 'mixed', 'mixed/quiet', 'top.txt']);
  assert.deepEqual(matchingFolders(root, 'different'), ['mixed']);
});

test('a change inside one is refreshed at the folder', () => {
  assert.equal(refreshTarget(root, 'equal/deep/file.txt'), 'equal');
  assert.equal(refreshTarget(root, 'mixed/b.txt'), 'mixed/b.txt');
});
