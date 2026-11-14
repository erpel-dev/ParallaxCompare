// The web view calls the app's commands by name with named arguments, and
// nothing in the build connects the two sides: a renamed command or argument
// fails only when the call is made. This test reads both and compares them.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const read = (path: string) => readFileSync(new URL(path, import.meta.url), 'utf8');

/** The index just past the bracket that closes the one at `open`. */
function closing(text: string, open: number): number {
  const pairs: Record<string, string> = { '(': ')', '{': '}', '[': ']', '<': '>' };
  const [a, b] = [text[open], pairs[text[open]]];
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    // `=>` and `->` are arrows, not brackets.
    if (text[i] === '>' && (text[i - 1] === '=' || text[i - 1] === '-')) continue;
    if (text[i] === a) depth++;
    else if (text[i] === b && --depth === 0) return i + 1;
  }
  throw new Error(`unbalanced ${a} at ${open}`);
}

/** Splits at commas that are not inside brackets. */
function parts(text: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < text.length; i++) {
    if ('({[<'.includes(text[i])) depth++;
    else if (')}]'.includes(text[i]) || (text[i] === '>' && text[i - 1] !== '=' && text[i - 1] !== '-')) depth--;
    else if (text[i] === ',' && depth === 0) {
      out.push(text.slice(start, i));
      start = i + 1;
    }
  }
  out.push(text.slice(start));
  return out.map((p) => p.trim()).filter(Boolean);
}

interface Command {
  /** Argument names as the web view sends them, and whether each may be left out. */
  args: Map<string, boolean>;
}

/** What the web view does not send: the app's state and handles. */
const injected = /^(State<|AppHandle|tauri::|Window|WebviewWindow)/;
const camel = (name: string) => name.replace(/_(\w)/g, (_, c: string) => c.toUpperCase());

function rustCommands(): Map<string, Command> {
  const out = new Map<string, Command>();
  for (const file of ['commands.rs', 'merge.rs']) {
    const text = read(`../src-tauri/src/${file}`);
    for (const m of text.matchAll(/#\[tauri::command\]\s*(?:#\[[^\n]*\]\s*)*pub(?:\([a-z]+\))? (?:async )?fn (\w+)\s*\(/g)) {
      const open = m.index + m[0].length - 1;
      const params = text.slice(open + 1, closing(text, open) - 1);
      const args = new Map<string, boolean>();
      for (const param of parts(params)) {
        const [name, type] = [param.slice(0, param.indexOf(':')).trim(), param.slice(param.indexOf(':') + 1).trim()];
        if (!injected.test(type)) args.set(camel(name.replace(/^mut /, '')), type.startsWith('Option<'));
      }
      out.set(m[1], { args });
    }
  }
  return out;
}

function registered(): Set<string> {
  const text = read('../src-tauri/src/main.rs');
  const open = text.indexOf('[', text.indexOf('generate_handler!'));
  return new Set(parts(text.slice(open + 1, closing(text, open) - 1)).map((p) => p.split('::').pop()!));
}

/** Each call of a command in the web view: its name and the argument names it sends. */
function calls(): { name: string; keys: string[] }[] {
  const text = read('../src/lib/api.ts');
  const out: { name: string; keys: string[] }[] = [];
  for (const m of text.matchAll(/\binvoke</g)) {
    const open = closing(text, m.index + m[0].length - 1);
    assert.equal(text[open], '(', `invoke call at ${m.index}`);
    const [name, object] = parts(text.slice(open + 1, closing(text, open) - 1));
    const keys = object ? parts(object.slice(1, -1)).map((entry) => entry.match(/^\w+/)![0]) : [];
    out.push({ name: name.replace(/"/g, ''), keys });
  }
  return out;
}

test('every command the web view calls exists, with the arguments it sends', () => {
  const commands = rustCommands();
  const handlers = registered();
  const all = calls();
  assert.ok(all.length > 50 && commands.size > 50, `read ${all.length} calls and ${commands.size} commands`);
  const problems: string[] = [];
  for (const { name, keys } of all) {
    const command = commands.get(name);
    if (!command) {
      problems.push(`${name}: no such command`);
      continue;
    }
    if (!handlers.has(name)) problems.push(`${name}: not registered in main.rs`);
    for (const key of keys) if (!command.args.has(key)) problems.push(`${name}: sends "${key}", which the command does not take`);
    for (const [arg, optional] of command.args) if (!optional && !keys.includes(arg)) problems.push(`${name}: does not send "${arg}"`);
  }
  assert.deepEqual(problems, []);
});

test('every command of the app is registered and called', () => {
  const called = new Set(calls().map((c) => c.name));
  const handlers = registered();
  const problems: string[] = [];
  for (const name of rustCommands().keys()) {
    if (!handlers.has(name)) problems.push(`${name}: not registered in main.rs`);
    if (!called.has(name)) problems.push(`${name}: never called by the web view`);
  }
  assert.deepEqual(problems, []);
});
