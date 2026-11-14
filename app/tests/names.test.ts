import { test } from 'node:test';
import assert from 'node:assert/strict';
import { revLabel, sourceName } from '../src/lib/api.ts';

test('versions are named as people know them', () => {
  assert.equal(revLabel('~staged'), 'staged');
  assert.equal(revLabel('main'), 'main');
  const staged = { type: 'gitRev', repo: '/home/me/work', rev: '~staged', subpath: 'src/cart.ts' } as const;
  assert.equal(sourceName(staged), 'cart.ts (staged)');
  assert.equal(sourceName(staged, 3), 'work/src/cart.ts (staged)');
  assert.equal(sourceName({ type: 'gitRev', repo: '/home/me/work', rev: 'v1.0', subpath: '' }), 'v1.0');
  assert.equal(sourceName({ type: 'local', path: '/home/me/work/src/cart.ts' }, 2), 'src/cart.ts');
});

test('control characters in names are shown as symbols', async () => {
  const { shown } = await import('../src/lib/format.ts');
  assert.equal(shown('new\nline.txt'), 'new␊line.txt');
  assert.equal(shown('tab\there.txt'), 'tab␉here.txt');
  assert.equal(shown('del\u007f'), 'del␡');
  assert.equal(shown('plain name.txt'), 'plain name.txt');
});
