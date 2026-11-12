import { test } from 'node:test';
import assert from 'node:assert/strict';
import { causeOf, summarizeErrors, exportErrors } from '../src/lib/errorSummary.ts';

const error = (name, message = 'Permission denied') => ({ name, status: 'error', message, left: null, right: null });
const root = (children) => ({ name: '', status: 'different', children, left: null, right: null });

test('root archive failures are counted and exported', () => {
  const failed = error('', 'bad.7z: Invalid archive');
  assert.deepEqual(summarizeErrors(failed), { count: 1, top: [['invalid archive', 1]], more: 0 });
  assert.equal(exportErrors(failed).text, '.\tbad.7z: Invalid archive');
});

test('causes keep messages without a path or separator', () => {
  assert.equal(causeOf('Permission denied (os error 13)'), 'permission denied');
  assert.equal(causeOf('C:\\a.7z: Storage full (os error 112)'), 'storage full');
  assert.equal(causeOf('Temporary drive is full while decompressing a.7z. Temporary directory: C:\\Temp. Free space or choose another temporary directory, then retry. (disk full (os error 112))'), 'temporary drive is full');
  assert.equal(causeOf('Could not use temporary storage while decompressing a.7z. Temporary directory: /tmp. Retry.'), 'could not use temporary storage');
  assert.equal(causeOf(undefined), 'could not be read');
  assert.equal(causeOf(''), 'could not be read');
});

test('large failure totals are independent of the bounded clipboard list', () => {
  const tree = root(Array.from({ length: 25000 }, (_, i) => error(`${i}.bin`)));
  assert.equal(summarizeErrors(tree)?.count, 25000);
  assert.deepEqual(summarizeErrors(tree)?.top, [['permission denied', 25000]]);
  const list = exportErrors(tree);
  assert.equal(list.total, 25000);
  assert.equal(list.copied, 20000);
  assert.equal(list.truncated, true);
  assert.match(list.text, /List truncated: 20,000 of 25,000/);
});

test('structured archive reasons keep their fields and explain common decoder errors', () => {
  assert.equal(causeOf('archive: UnsupportedVersion { major: 99, minor: 111 }'),
    'unsupported 7z version 99.111 (unsupported or corrupt archive)');
  assert.equal(causeOf('a.7z: DecoderError { reason: BadHeader, index: 3 }'),
    'decoderError { reason: BadHeader, index: 3 }');
  assert.equal(causeOf('archive: invalid Zip archive: Could not find EOCD'),
    'incomplete or corrupt ZIP archive (end-of-directory record missing)');
  assert.equal(causeOf('C:\\[backup]\\a.7z: Permission denied (os error 13)'), 'permission denied');
});

test('clipboard character budget and deep trees are handled without recursion', () => {
  const list = exportErrors(root([error('large', 'x'.repeat(1000)), error('small')]), 20, 100);
  assert.equal(list.copied, 0);
  assert.equal(list.total, 2);
  assert.equal(list.truncated, true);
  let tree = error('leaf');
  for (let i = 0; i < 10000; i++) tree = { ...root([tree]), name: 'nested' };
  assert.equal(summarizeErrors(tree)?.count, 1);
  assert.equal(summarizeErrors(root([])), null);
});
