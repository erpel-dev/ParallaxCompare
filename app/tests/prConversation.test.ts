import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { compileModule } from 'svelte/compiler';
import ts from 'typescript';

// Run the actual rune store, replacing only native I/O. Each request is independently controlled.
const source = await readFile(new URL('../src/lib/prConversation.svelte.ts', import.meta.url), 'utf8');
const js = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ESNext, module: ts.ModuleKind.ESNext } }).outputText;
const compiled = compileModule(js.replace(/import \{ api \} from "\.\/api";/, 'const api = globalThis.__prTest.api;')
  .replace(/import \{ notify \} from "\.\/toast.svelte";/, 'const notify = () => {};'), { filename: 'prConversation.svelte.js', generate: 'client' }).js.code;
const cache = new URL('../node_modules/.cache/pr-state/', import.meta.url);
await mkdir(cache, { recursive: true });
let sequence = 0;
const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
const settle = () => new Promise((resolve) => setImmediate(resolve));
const conversation = (extra = {}) => ({ url: 'https://github.com/fixture/repo/pull/1', head: 'head', pinnedHead: 'head', threads: [], files: {}, annotations: [], checks: { passed: 0, failed: 0, pending: 0 }, ...extra });
const details = { checks: { passed: 50, failed: 0, pending: 0 }, annotations: [], checksTruncated: true, annotationsTruncated: true };
async function fixture() {
  const rounds = [], calls = [];
  let active;
  const api = { ...Object.fromEntries(['Conversation', 'Checks', 'Files'].map((part) => ['pullRequest' + part, () => { calls.push(part); return active[part].promise; }])), pullRequestWrite: async () => { calls.push('Write'); } };
  globalThis.__prTest = { api };
  const path = new URL(`store-${process.pid}-${sequence++}.mjs`, cache);
  await writeFile(path, compiled);
  const { prConversations: store, needsSignIn } = await import(path.href);
  const round = () => { active = Object.fromEntries(['Conversation', 'Checks', 'Files'].map((part) => [part, deferred()])); rounds.push(active); return active; };
  const first = round();
  store.register(1, { url: 'https://github.com/fixture/repo/pull/1' }, 'head');
  return { store, first, round, calls, needsSignIn };
}

test('comment errors are visible while independent CI and file requests are still pending', async () => {
  const { store, first, calls } = await fixture();
  assert.deepEqual(calls, ['Conversation', 'Checks', 'Files']);
  first.Conversation.reject('comments denied'); await settle();
  assert.equal(store.get(1).error, 'comments denied');
  assert.equal(store.get(1).loading, false);
  assert.equal(store.get(1).checksLoading, true);
  assert.equal(store.get(1).filesLoading, true);
  first.Checks.resolve(details); first.Files.resolve({ files: {}, truncated: false }); await settle();
});

test('optional failures are explicit and do not hide a successful conversation', async () => {
  const { store, first } = await fixture();
  first.Conversation.resolve(conversation()); first.Checks.reject('CI denied'); first.Files.reject('files denied'); await settle();
  const state = store.get(1);
  assert.ok(state.data); assert.equal(state.error, null);
  assert.equal(state.checksError, 'CI denied'); assert.equal(state.filesError, 'files denied');
  assert.equal(state.checksLoading, false); assert.equal(state.filesLoading, false);
});

test('early optional results survive comments and moved heads cannot enable line comments', async () => {
  const { store, first, round } = await fixture();
  first.Checks.resolve(details); first.Files.resolve({ files: { 'a.rs': { left: [], right: [[1, 2]] } }, truncated: false }); await settle();
  first.Conversation.resolve(conversation()); await settle();
  assert.equal(store.get(1).data.checks.passed, 50); assert.equal(store.get(1).checksTruncated, true);
  assert.ok(store.get(1).data.files['a.rs']);
  const next = round(); const loaded = store.load(1);
  assert.deepEqual(Object.keys(store.get(1).data.files), []);
  next.Files.resolve({ files: { 'a.rs': {} }, truncated: false }); next.Checks.resolve(details);
  next.Conversation.resolve(conversation({ head: 'new-head' })); await loaded;
  assert.deepEqual(Object.keys(store.get(1).data.files), []);
});

test('closing a session clears retained data and late results cannot recreate it', async () => {
  const { store, first } = await fixture();
  store.focus = { session: 1, path: 'a' }; store.replyTo = { session: 1, thread: 0 };
  store.composer = { session: 1, path: 'a', side: 'right', start: 1, end: 1 }; store.reviewing = 1; store.busy[1] = true;
  store.release(1);
  first.Conversation.resolve(conversation()); first.Checks.resolve(details); first.Files.resolve({ files: {}, truncated: false }); await settle();
  assert.equal(store.get(1), undefined); assert.equal(store.busy[1], undefined);
  assert.equal(store.focus, null); assert.equal(store.replyTo, null); assert.equal(store.composer, null); assert.equal(store.reviewing, null);
});

test('a newer refresh wins when old requests complete out of order', async () => {
  const { store, first, round } = await fixture();
  const second = round(); const loaded = store.load(1);
  second.Conversation.resolve(conversation({ title: 'new' })); second.Checks.resolve(details); second.Files.resolve({ files: {}, truncated: false }); await loaded;
  first.Conversation.resolve(conversation({ title: 'old' })); first.Checks.reject('old error'); first.Files.reject('old error'); await settle();
  assert.equal(store.get(1).data.title, 'new'); assert.equal(store.get(1).checksError, null); assert.equal(store.get(1).filesError, null);
});

test('sequential reply and resolve wait for fresh comments but never optional CI', async () => {
  const { store, first, round, calls } = await fixture();
  first.Conversation.resolve(conversation()); first.Checks.resolve(details); first.Files.resolve({ files: {}, truncated: false }); await settle();
  const replyRefresh = round();
  const reply = store.write(1, { kind: 'reply', thread: 'T0', comment: 100, body: 'test', review: null });
  await settle(); assert.equal(store.get(1).loading, true);
  replyRefresh.Conversation.resolve(conversation()); assert.equal(await reply, true);
  assert.equal(store.get(1).checksLoading, true);
  const resolveRefresh = round();
  const resolved = store.write(1, { kind: 'resolve', thread: 'T0', resolved: true });
  await settle(); resolveRefresh.Conversation.resolve(conversation()); assert.equal(await resolved, true);
  assert.equal(calls.filter((c) => c === 'Write').length, 2);
  for (const r of [replyRefresh, resolveRefresh]) { r.Checks.resolve(details); r.Files.resolve({ files: {}, truncated: false }); }
  await settle();
});

test('generic GitHub access advice cannot turn an unrelated read error into a sign-in diagnosis', async () => {
  const { store, first, needsSignIn } = await fixture();
  const advice = '. Check repository access and run `gh auth login --hostname github.com`.';
  assert.equal(needsSignIn('gh request: temporary outage' + advice), false);
  assert.equal(needsSignIn('helper output exceeded its memory limit' + advice), false);
  assert.equal(needsSignIn('HTTP 401: Bad credentials' + advice), true);
  assert.equal(needsSignIn('To get started, run gh auth login' + advice), true);
  store.release(1); first.Conversation.resolve(conversation()); first.Checks.resolve(details); first.Files.resolve({ files: {}, truncated: false }); await settle();
});
