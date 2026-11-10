import { test } from 'node:test';
import assert from 'node:assert/strict';
import { RowProjection } from '../src/lib/rowFilter.ts';
import { flatten, flatFiles, movedPairs, matchingFolders, matchingCount, countFiles, countFlatFiles } from '../src/lib/tree.ts';

const side = { kind: 'file', size: 1, mtime: null };
const file = (name, status) => ({ name, status, left: status === 'rightOnly' ? null : side, right: status === 'leftOnly' ? null : side });
const dir = (name, children) => ({ name, status: 'different', left: { ...side, kind: 'dir' }, right: { ...side, kind: 'dir' }, children });
const root = dir('', [dir('only-one-side', [file('l.txt', 'leftOnly'), file('r.txt', 'rightOnly')]), dir('nested', [dir('inner', [file('changed.txt', 'different'), file('same.txt', 'same'), file('similar.txt', 'similar'), file('bad.txt', 'error')])])]);
const all = new Set(['only-one-side', 'nested', 'nested/inner']);

test('exact filters retain ancestors and exclude aggregate folder false positives', () => {
  for (const [filter, expected] of [['different','changed.txt'],['leftOnly','l.txt'],['rightOnly','r.txt'],['same','same.txt'],['similar','similar.txt'],['error','bad.txt']]) {
    const rows = flatten(root, all, filter);
    assert.deepEqual(rows.filter(r => !r.container).map(r => r.node.name), [expected]);
    assert.equal(matchingCount(countFiles(root), filter), 1);
  }
  assert.deepEqual(flatten(root, all, 'different').map(r => r.path), ['nested', 'nested/inner', 'nested/inner/changed.txt']);
  assert.deepEqual(matchingFolders(root, 'different'), ['nested', 'nested/inner']);
});
test('broad differences include one-sided and errors; Same excludes Similar', () => {
  assert.equal(matchingCount(countFiles(root), 'diffs'), 4);
  assert.equal(flatten(root, all, 'diffs').filter(r => !r.container).length, 4);
  assert.equal(flatten(root, all, 'orphans').filter(r => !r.container).length, 2);
  assert.equal(countFiles(root).files, 6);
});
test('pending rows remain visible without inflating stable completed counts', () => {
  const pending = dir('', [file('working.txt','pending')]);
  assert.equal(flatten(pending, new Set(), 'leftOnly').length, 1);
  assert.equal(countFiles(pending).files, 0);
});
test('archives compared as files and failed folders match their own status', () => {
  const archive = { ...file('data.zip','different'), left:{...side,kind:'archive'},right:{...side,kind:'archive'},unexpanded:true };
  const failed = { ...dir('denied',[]), status:'error' };
  const r = dir('',[archive,failed]);
  assert.equal(countFiles(r).different, 1);
  assert.deepEqual(flatten(r,new Set(),'different').map(r=>r.path),['data.zip']);
  assert.deepEqual(flatten(r,new Set(),'error').map(r=>r.path),['denied']);
});
test('flat view totals count a moved pair once and use its resolved status', () => {
  const root = dir('', [dir('old',[file('moved.txt','leftOnly')]),dir('new',[file('moved.txt','rightOnly')]),file('real-left.txt','leftOnly')]);
  const moved = movedPairs(root);
  const pending = flatFiles(root,'all',moved,new Map());
  assert.equal(countFlatFiles(pending).files,1);
  assert.equal(pending.filter(r=>r.pending).length,1);
  const rows = flatFiles(root,'all',moved,new Map([['old/moved.txt\nnew/moved.txt',{status:'same',message:null}]]));
  assert.equal(countFlatFiles(rows).files,2);
  assert.equal(countFlatFiles(rows).same,1);
  assert.equal(countFlatFiles(rows).leftOnly,1);
  assert.equal(countFlatFiles(rows).rightOnly,0);
  assert.equal(flatFiles(root,'leftOnly',moved,new Map([['old/moved.txt\nnew/moved.txt',{status:'same',message:null}]])).length,1);
});
const runs = [{start:0,end:100,kind:'equal'},{start:100,end:102,kind:'changed'},{start:102,end:103,kind:'removed'},{start:103,end:200,kind:'equal'},{start:200,end:203,kind:'added'},{start:203,end:204,kind:'minor'},{start:204,end:205,kind:'changed'}];
test('filtered rows keep original IDs, with one fold bar per hidden stretch', () => {
  const p = new RowProjection(runs,'changed');
  // fold(0..100) 100 101 fold(102..204) 204
  assert.equal(p.length,5);
  assert.equal(p.folds,2);
  assert.deepEqual([0,1,2,3,4,5,-1].map(i=>p.source(i)),[null,100,101,null,204,null,null]);
  assert.deepEqual(p.fold(0),{start:0,end:100});
  assert.deepEqual(p.fold(3),{start:102,end:204});
  assert.equal(p.fold(1),null);
  for(const [row,position,contains] of [[0,0,false],[100,1,true],[101,2,true],[102,3,false],[203,3,false],[204,4,true],[205,5,false]]) {
    assert.equal(p.position(row),position);assert.equal(p.contains(row),contains);
  }
  // Moving past a fold bar lands on the next row.
  assert.equal(p.rowNear(0,1),100);
  assert.equal(p.rowNear(3,1),204);
  assert.equal(p.rowNear(3,-1),101);
});
test('context keeps rows around matches and merges what touches', () => {
  const p = new RowProjection(runs,'changed',3);
  // 97..105 (100,101 with 3 around; 102,103 are context too), then 201..205
  assert.deepEqual([0,1,2].map(i=>p.source(i)),[null,97,98]);
  assert.equal(p.contains(99),true);
  assert.equal(p.contains(104),true);
  assert.equal(p.contains(105),false);
  assert.equal(p.contains(201),true);
  const whole = new RowProjection(runs,'diffs',1000);
  assert.equal(whole.length,205);
  assert.equal(whole.folds,0);
  // An opened stretch shows although the filter hides it.
  const opened = new RowProjection(runs,'changed',0,[{start:0,end:20}]);
  assert.equal(opened.source(0),0);
  assert.equal(opened.contains(19),true);
  assert.deepEqual(opened.fold(20),{start:20,end:100});
});
test('all difference kinds combine, while minor rows stay separate', () => {
  const rows = (p) => p.length - p.folds;
  assert.equal(new RowProjection(runs,'all').length,205);
  assert.equal(new RowProjection(runs,'all',3).folds,0);
  assert.equal(rows(new RowProjection(runs,'diffs')),7);
  assert.equal(rows(new RowProjection(runs,'minor')),1);
  assert.equal(new RowProjection(runs,'removed').rowNear(0,1),102);
  assert.equal(new RowProjection([], 'changed').source(0),null);
  assert.equal(new RowProjection([{start:0,end:9,kind:'equal'}], 'changed').length,0);
});
test('large files keep compact ranges rather than per-row indices', () => {
  const p = new RowProjection([{start:0,end:100_000_000,kind:'equal'},{start:100_000_000,end:100_000_002,kind:'added'}],'all');
  assert.equal(p.parts.length,1);
  assert.equal(p.length,100_000_002);
  assert.equal(p.source(100_000_001),100_000_001);
  assert.equal(p.position(100_000_002),100_000_002);
});

test('moved-file groups remain paired across the bounded matching threshold', () => {
  for (const n of [100, 141, 142, 150, 5000]) {
    const entries = [];
    for (let i = 0; i < n; i++) {
      const suffix = String(i).padStart(5, '0');
      entries.push(dir(`old${suffix}`, [file('index.txt', 'leftOnly')]));
      entries.push(dir(`new${suffix}`, [file('index.txt', 'rightOnly')]));
    }
    const pairs = movedPairs(dir('', entries));
    assert.equal(pairs.length, n);
    assert.equal(new Set(pairs.map(([l]) => l)).size, n);
    assert.equal(new Set(pairs.map(([,r]) => r)).size, n);
    assert.deepEqual(pairs[0], ['old00000/index.txt', 'new00000/index.txt']);
    assert.deepEqual(movedPairs(dir('', [...entries].reverse())), pairs);
  }
});

test('bounded matching prioritizes common suffixes then pairs remaining unequal groups', () => {
  const entries = [];
  for (let i = 0; i < 150; i++) {
    entries.push(dir(`old${i}`, [file('index.txt', 'leftOnly')]));
    if (i < 149) entries.push(dir(`new${i}`, [file('index.txt', 'rightOnly')]));
  }
  entries.push(dir('left/shared', [file('index.txt','leftOnly')]));
  entries.push(dir('right/shared', [file('index.txt','rightOnly')]));
  const pairs = movedPairs(dir('', entries));
  assert.equal(pairs.length, 150);
  assert.ok(pairs.some(([l,r]) => l === 'left/shared/index.txt' && r === 'right/shared/index.txt'));
});
