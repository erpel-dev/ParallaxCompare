import { test } from 'node:test';
import assert from 'node:assert/strict';
import { renderMarkdown } from '../src/lib/markdown.ts';

test('comment markdown never becomes raw HTML', () => {
  const r = renderMarkdown('<img src=x onerror=alert(1)> <script>bad()</script>\n\n**bold** and `<b>`');
  assert.ok(!r.html.includes('<img'), r.html);
  assert.ok(!r.html.includes('<script'), r.html);
  assert.ok(r.html.includes('&lt;script&gt;'), r.html);
  assert.ok(r.html.includes('<strong>bold</strong>'), r.html);
  assert.ok(r.html.includes('<code>&lt;b&gt;</code>'), r.html);
});

test('links are https only and images are not loaded', () => {
  const r = renderMarkdown('[ok](https://github.com/a/b) [js](javascript:alert(1)) ![pic](https://x.example/p.png)');
  assert.ok(r.html.includes('data-href="https://github.com/a/b"'), r.html);
  assert.ok(!r.html.includes('javascript:'.replace(':', '') + '" '), r.html);
  assert.ok(!/href="javascript/.test(r.html), r.html);
  assert.ok(!r.html.includes('<img'), r.html);
  assert.ok(r.html.includes('[image: pic]'), r.html);
  // Nothing has a real href, so a click cannot navigate the app's window.
  assert.ok(!/\shref=/.test(r.html), r.html);
});

test('suggestions, code, lists and quotes', () => {
  const r = renderMarkdown('Use this:\n```suggestion\n    let x = 1;\n```\n- one\n- two\n\n> quoted\n\n```rust\nfn a() {}\n```');
  assert.deepEqual(r.suggestions, [{ lines: ['    let x = 1;'] }]);
  assert.ok(r.html.includes('class="md-suggestion"'), r.html);
  assert.ok(r.html.includes('<ul><li>one</li><li>two</li></ul>'), r.html);
  assert.ok(r.html.includes('<blockquote><p>quoted</p></blockquote>'), r.html);
  assert.ok(r.html.includes('<pre><code>fn a() {}</code></pre>'), r.html);
});
