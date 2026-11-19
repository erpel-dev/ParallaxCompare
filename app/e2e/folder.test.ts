// Smoke tests of the folder and text views in a real browser engine, against the stand-in
// backend in backend.js. They cover what unit tests of helpers cannot: that the
// view asks the backend for the right things and shows what it gets.
// Run with `npm run e2e` after `npm run build`.
import { after, before, beforeEach, test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer, type Server } from 'node:http';
import { readFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { extname, join, normalize } from 'node:path';
import { chromium, type Browser, type Page } from 'playwright';

const dist = new URL('../dist/', import.meta.url).pathname;
const types: Record<string, string> = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.wasm': 'application/wasm' };

let server: Server;
let browser: Browser;
let page: Page;
let origin = '';

before(async () => {
  assert.ok(existsSync(join(dist, 'index.html')), 'build the web view first: npm run build');
  server = createServer(async (request, response) => {
    const path = normalize(decodeURIComponent(new URL(request.url ?? '/', 'http://x').pathname));
    try {
      const body = await readFile(join(dist, path === '/' ? 'index.html' : path));
      response.writeHead(200, { 'content-type': types[extname(path)] ?? (path === '/' ? 'text/html' : 'application/octet-stream') }).end(body);
    } catch {
      response.writeHead(404).end();
    }
  });
  await new Promise<void>((done) => server.listen(0, '127.0.0.1', done));
  const address = server.address();
  origin = `http://127.0.0.1:${typeof address === 'object' && address ? address.port : 0}`;
  // PLAYWRIGHT_CHROMIUM: a browser installed elsewhere than Playwright's own.
  browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM || undefined });
});

after(async () => {
  await browser?.close();
  server?.close();
});

beforeEach(async () => {
  await page?.close();
  page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
  page.on('pageerror', (error) => assert.fail(`error in the page: ${error.message}`));
  await page.addInitScript({ path: new URL('./backend.js', import.meta.url).pathname });
  await page.goto(origin);
  await page.getByText('Showing').waitFor();
});

const rows = () => page.getByRole('treeitem');
const row = (name: string) => rows().filter({ hasText: name }).first();
/** The names shown, one per row: the left side's, without the icon and the counts beside it. */
const names = () =>
  rows().evaluateAll((list) =>
    list.map((r) => {
      const name = r.querySelector('.name')!.cloneNode(true) as HTMLElement;
      name.querySelectorAll('span').forEach((s) => s.remove());
      return name.textContent!.trim();
    }),
  );
const count = (command: string) => page.evaluate((c) => (window as any).__backend.count(c), command);
const calls = (command: string) => page.evaluate((c) => (window as any).__backend.calls.filter((x: any) => x.command === c).map((x: any) => x.args), command);
/** A button of the filter bar. */
const filter = (label: string) => page.locator('button[aria-pressed]').filter({ hasText: label });
const statusLine = () => page.locator('.status').innerText();

test('shows the result, with identical folders closed and counted', async () => {
  const shown = await names();
  // Folders with differences are open; the identical one is a single row.
  for (const name of ['equal', 'mixed', 'quiet', 'changed.txt', 'left-only.txt', 'same.txt', 'odd', 'top.txt']) {
    assert.ok(shown.some((n) => n.startsWith(name)), `${name} in ${JSON.stringify(shown)}`);
  }
  assert.ok(!shown.some((n) => n.startsWith('e00.txt')), 'entries of the identical folder are not loaded');
  // 15 in equal, 5 in mixed, 2 in odd, 1 at the top: also those not loaded.
  assert.match(await statusLine(), /Showing 23 of 23 files/);
  assert.equal(await count('run_compare'), 1);
  assert.equal(await count('expand'), 0);
});

test('loads the entries of an identical folder when it is opened', async () => {
  await row('equal').locator('.chev').first().click();
  await row('e00.txt').waitFor();
  assert.deepEqual((await calls('expand')).map((a) => [a.path, a.deep]), [['equal', false]]);
  // The folder inside it is identical too: a row, loaded when opened.
  assert.equal(await row('i00.txt').count(), 0);
  await row('inner').locator('.chev').first().click();
  await row('i00.txt').waitFor();
  assert.deepEqual((await calls('expand')).map((a) => a.path), ['equal', 'equal/inner']);
  assert.match(await statusLine(), /Showing 23 of 23 files/);
});

test('shows control characters in names as symbols', async () => {
  const shown = await names();
  assert.ok(shown.includes('new␊line.txt') && shown.includes('tab␉here.txt'), JSON.stringify(shown));
});

test('the selection bar counts the selected rows; a filter starts over', async () => {
  const bar = page.getByRole('toolbar', { name: 'Selected entries' });
  await row('changed.txt').click();
  await bar.getByText('1 row selected').waitFor();
  await row('same.txt').click({ modifiers: ['Shift'] });
  await bar.getByText('3 rows selected').waitFor();
  // Another filter shows other rows: no bar, not a count of rows that are gone.
  await filter('Right only').click();
  await bar.waitFor({ state: 'detached' });
  assert.deepEqual(await names(), ['odd', 'tab␉here.txt']);
});

test('deleting a row removes it and the selection with it', async () => {
  await row('left-only.txt').click();
  await page.getByRole('button', { name: 'Delete on the left…' }).click();
  await page.getByRole('button', { name: /^Move to / }).click();
  await row('left-only.txt').waitFor({ state: 'detached' });
  assert.deepEqual((await calls('delete_entries')).map((a) => [a.paths, a.left]), [[['mixed/left-only.txt'], true]]);
  await page.getByRole('toolbar', { name: 'Selected entries' }).waitFor({ state: 'detached' });
  assert.match(await statusLine(), /Showing 22 of 22 files/);
});

test('flat view asks for everything once', async () => {
  await page.getByRole('button', { name: 'Flat', exact: true }).click();
  await page.getByText('e00.txt').first().waitFor();
  const deep = (await calls('expand')).filter((a) => a.deep);
  assert.ok(deep.length >= 1 && deep.length <= 3, JSON.stringify(deep));
  assert.match(await statusLine(), /Showing 23 of 23 files/);
});

test('a superseded find-side search leaves the row filter and selection in place', async () => {
  await row('changed.txt').dblclick();
  await page.getByText('needle', { exact: true }).first().waitFor();
  await page.keyboard.press('Control+f');
  await page.getByPlaceholder('Find in both files').fill('needle');
  const matchCount = page.getByText('1 of 1', { exact: true });
  await matchCount.waitFor();

  const same = page.getByRole('group', { name: 'Filter comparison by status' }).getByRole('button', { name: /^Same\b/ });
  const selected = page.locator('.row.cur .code[data-side=right]');
  await same.click();
  assert.equal(await selected.innerText(), 'shared');

  const sides = page.getByRole('group', { name: 'Side to search' });
  const right = sides.getByRole('button', { name: 'Right', exact: true });
  const left = sides.getByRole('button', { name: 'Left', exact: true });
  const finishSearch = (side: 'left' | 'right') => page.evaluate((s) => (window as any).__backend.finishSearch(s), side);

  await right.click();
  await left.click();
  await finishSearch('right');
  await finishSearch('left');
  await page.getByText('No matches', { exact: true }).waitFor();
  assert.equal(await same.getAttribute('aria-pressed'), 'true');
  assert.equal(await selected.innerText(), 'shared');

  await right.click();
  await finishSearch('right');
  await matchCount.waitFor();
  assert.equal(await same.getAttribute('aria-pressed'), 'false');
  assert.equal(await selected.innerText(), 'needle');
});
