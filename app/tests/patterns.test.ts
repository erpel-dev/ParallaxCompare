import { test } from 'node:test';
import assert from 'node:assert/strict';
import { splitPatterns, joinPatterns } from '../src/lib/patterns.ts';
import { copyFilesBelow } from '../src/lib/tree.ts';

test('pattern separators preserve glob syntax and quoted or escaped filenames', () => {
  assert.deepEqual(splitPatterns('*.{java,kt}; src/**/{a,b}.txt [a,; ].txt'), ['*.{java,kt}', 'src/**/{a,b}.txt', '[a,; ].txt']);
  assert.deepEqual(splitPatterns('"report final.txt" "report,final.txt" \'report;final.txt\''), ['report final.txt', 'report,final.txt', 'report;final.txt']);
  assert.deepEqual(splitPatterns(String.raw`report\ final.txt report\,final.txt O'Brien.txt literal\?.txt`), ['report final.txt', 'report,final.txt', "O'Brien.txt", String.raw`literal\?.txt`]);
  assert.deepEqual(splitPatterns('*.{o,obj}, report final.txt, "report,final.txt"', true), ['*.{o,obj}', 'report final.txt', 'report,final.txt']);
  assert.throws(() => splitPatterns('"unfinished'), /Close the quote/);
});

test('copy preview treats expanded archives atomically and respects the source side', () => {
  const file = { kind: 'file', size: 1, mtime: null };
  const archive = { name: 'bundle.zip', status: 'same', left: {...file,kind:'archive'}, right: {...file,kind:'archive'}, children: [{name:'keep.txt',status:'same',left:file,right:file}] };
  const root = { name:'wrapper',status:'different',left:{...file,kind:'dir'},right:{...file,kind:'dir'},children:[archive] };
  assert.deepEqual(copyFilesBelow(root, 'wrapper', true).map(f=>f.path), ['wrapper/bundle.zip']);
  const mixed = {...archive,left:{...file,kind:'dir'}};
  assert.deepEqual(copyFilesBelow(mixed,'bundle.zip',true).map(f=>f.path), ['bundle.zip/keep.txt']);
  assert.deepEqual(copyFilesBelow(mixed,'bundle.zip',false).map(f=>f.path), ['bundle.zip']);
});

test('saved pattern lists retain separators and glob escapes when displayed again', () => {
  const patterns = ['*.{java,kt}', 'report final.txt', 'report,final.txt', 'report;final.txt', String.raw`literal\?.txt`, 'with"quote.txt'];
  for (const excludes of [true, false]) assert.deepEqual(splitPatterns(joinPatterns(patterns, excludes), excludes), patterns);
});
