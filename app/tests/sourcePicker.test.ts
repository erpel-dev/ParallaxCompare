import { test } from "node:test";
import assert from "node:assert/strict";
import { pickerFor, restorePicker, shapeMismatch, toSpec, usable } from "../src/lib/sourcePicker.ts";
import { api } from "../src/lib/api.ts";

test("saved four-tab sources migrate without losing revision, scope or worktree intent", () => {
  const rev = { type: "gitRev", repo: "/repo", rev: "feature/new", subpath: "docs" };
  assert.deepEqual(
    restorePicker({ kind: "gitRev", path: "/repo", rev: "feature/new", subpath: "docs" }),
    pickerFor(rev),
  );
  assert.deepEqual(
    restorePicker({ kind: "gitRev", path: "/repo", rev: "" }),
    pickerFor({ type: "gitRev", repo: "/repo", rev: "HEAD", subpath: "" }),
  );
  assert.deepEqual(
    restorePicker({ kind: "gitWorktree", path: "/repo" }),
    pickerFor({ type: "gitWorktree", repo: "/repo" }),
  );
  assert.deepEqual(restorePicker({ kind: "file", path: "/a.zip" }), { path: "/a.zip" });
  for (const saved of [null, "broken", { path: 4 }, {}]) assert.deepEqual(restorePicker(saved), { path: "" });
});

test("explicit saved source intent is accepted only when it matches the displayed text", async () => {
  const spec = { type: "gitRev", repo: "/repo", rev: "main", subpath: "" };
  const chosen = pickerFor(spec);
  assert.deepEqual(await toSpec(chosen), spec);
  assert.deepEqual(restorePicker(chosen), chosen);
  assert.deepEqual(restorePicker({ ...chosen, path: "/other" }), { path: "/other" });
  assert.deepEqual(restorePicker({ path: "bad", spec: { type: "gitRev", repo: 1, rev: null } }), { path: "bad" });
  const original = api.parseSource;
  let parsed = "";
  try {
    api.parseSource = async (text) => {
      parsed = text;
      return { type: "local", path: text };
    };
    assert.deepEqual(await toSpec({ path: " /other ", spec }), { type: "local", path: "/other" });
    assert.equal(parsed, "/other");
  } finally {
    api.parseSource = original;
  }
});

test("Compare requires a current successful preview; remote access stays deferred", () => {
  const value = { path: "/folder" };
  const preview = { input: "/folder", kind: "folder", isFile: false };
  assert.equal(usable(preview, value), true);
  assert.equal(usable(preview, { path: "/changed" }), false);
  assert.equal(usable(null, value), false);
  for (const kind of ["empty", "error", "needsRepository"]) assert.equal(usable({ ...preview, kind }, value), false);
  assert.equal(usable({ ...preview, kind: "remote", isFile: null }, value), true);
});

test("shape checks block known file/container conflicts without guessing unresolved shapes", () => {
  const file = { isFile: true },
    folder = { isFile: false },
    unknown = { isFile: null };
  assert.equal(shapeMismatch(file, folder), true);
  assert.equal(shapeMismatch(folder, file), true);
  for (const pair of [
    [file, file],
    [folder, folder],
    [file, unknown],
    [null, folder],
  ])
    assert.equal(shapeMismatch(...pair), false);
});
