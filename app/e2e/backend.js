// A stand-in for the app's backend, loaded into the page before the web view's
// own code. It keeps two small trees, answers the commands the folder view
// uses the way the app does (identical folders come without their entries),
// and serves a text comparison for changed.txt. Tests can inspect the calls.
(() => {
  const file = (size = 4) => ({ kind: "file", size, mtime: 1700000000 });
  const dir = () => ({ kind: "dir", size: 0, mtime: 1700000000 });
  const names = (n, prefix) => Array.from({ length: n }, (_, i) => `${prefix}${String(i).padStart(2, "0")}.txt`);
  const pair = (name, status = "same") => ({ name, left: file(), right: file(status === "same" ? 4 : 9), status });
  const folder = (name, children) => ({ name, left: dir(), right: dir(), status: "same", children });

  // equal/: identical, with a folder inside. mixed/: one changed file, one only
  // on the left, an identical folder. odd/: names with a tab and a line break.
  const matched = new URLSearchParams(location.search).has("matched");
  const movedNames = ["same\nname.txt", "changed-move.txt"];
  const orphan = (name, left) => ({ name, left: left ? file() : null, right: left ? null : file(), status: left ? "leftOnly" : "rightOnly" });
  const root = matched ? folder("", [
    folder("aaa", [pair("line-endings.txt", "similar")]),
    folder("before", movedNames.map((name) => orphan(name, true))),
    folder("after", movedNames.map((name) => orphan(name, false))),
  ]) : folder("", [
    folder("equal", [folder("inner", names(3, "i").map((n) => pair(n))), ...names(12, "e").map((n) => pair(n))]),
    folder("mixed", [
      folder("quiet", names(2, "q").map((n) => pair(n))),
      pair("changed.txt", "different"),
      { name: "left-only.txt", left: file(), right: null, status: "leftOnly" },
      pair("same.txt"),
    ]),
    folder("odd", [
      { name: "new\nline.txt", left: file(), right: null, status: "leftOnly" },
      { name: "tab\there.txt", left: null, right: file(), status: "rightOnly" },
    ]),
    pair("top.txt"),
  ]);

  const container = (n) => n.left?.kind === "dir" || n.right?.kind === "dir";
  function settle(node) {
    if (!node.children) return;
    node.children.forEach(settle);
    if (node.left && node.right) {
      node.status = node.children.every((c) => c.status === "same") ? "same" : "different";
    }
  }
  settle(root);

  const find = (path) => path.split("/").filter(Boolean).reduce((n, part) => n?.children?.find((c) => c.name === part), root);
  const filesBelow = (n) => (n.children ?? []).reduce((sum, c) => sum + (container(c) ? filesBelow(c) : 1), 0);
  const shallow = ({ children, ...rest }) => ({ ...rest });
  /** A node as its folder's listing holds it: an identical folder without its entries. */
  const entry = (n) =>
    container(n) && n.status === "same" && n.children?.length ? { ...shallow(n), unexpanded: true, sameFiles: filesBelow(n) } : view(n, false);
  const view = (n, deep) => ({ ...shallow(n), ...(n.children?.length ? { children: n.children.map((c) => (deep ? view(c, true) : entry(c))) } : {}) });

  function remove(path, left) {
    const parent = find(path.split("/").slice(0, -1).join("/"));
    const node = find(path);
    if (!parent || !node) return null;
    const strip = (n) => {
      if (left) n.left = null;
      else n.right = null;
      n.status = n.left ? "leftOnly" : "rightOnly";
      n.children?.forEach(strip);
    };
    strip(node);
    if (!node.left && !node.right) parent.children = parent.children.filter((c) => c !== node);
    settle(root);
    return find(path) ? entry(find(path)) : null;
  }

  const source = (path) => ({ kind: "folder", display: path, rev: null, commit: null, commitSummary: null, writable: true });
  const summary = () => {
    const s = { files: 0, same: 0, similar: 0, different: 0, leftOnly: 0, rightOnly: 0, errors: 0, identicalSubtrees: 0, elapsedMs: 1 };
    const walk = (n) => (n.children ?? []).forEach((c) => (container(c) ? walk(c) : (s.files++, s[c.status]++)));
    walk(root);
    return s;
  };

  const line = (n, t) => ({ n, t, s: [], m: [] });
  const fileRows = [
    { k: "equal", l: line(0, "shared"), r: line(0, "shared") },
    { k: "changed", l: line(1, "different"), r: line(1, "needle") },
  ];
  const fileSide = { exists: true, size: 14, binary: false, encoding: "UTF-8", lineEnding: "LF", bom: false, lines: 2, editable: false };
  const pendingSearches = new Map();
  const listeners = new Map();
  const changedPairs = new Set(["changed-move.txt"]);
  let holdRefresh = false;
  let rejectRefresh;

  const calls = [];
  const commands = {
    app_info: () => ({ version: "0.0.0", revision: "test", os: "linux", arch: "x86_64", tempDir: "/tmp" }),
    startup_request: () => ({ kind: "compare", left: { type: "local", path: "/work/left" }, right: { type: "local", path: "/work/right" }, mergeBase: false }),
    open_session: ({ left, right }) => ({
      id: left.path === "/work/left" ? 1 : 3,
      left: source(left.path),
      right: source(right.path),
      isFile: left.path.endsWith(".txt"),
    }),
    close_session: () => null,
    side_source: ({ left, rel }) => ({ type: "local", path: `/work/${left ? "left" : "right"}/${rel}` }),
    run_compare: ({ invalidate }) => {
      if (invalidate && holdRefresh) {
        holdRefresh = false;
        return new Promise((_, reject) => (rejectRefresh = reject));
      }
      return { root: view(root, false), summary: summary() };
    },
    cancel_compare: () => rejectRefresh?.("cancelled"),
    compare_pairs: ({ pairs }) => pairs.map(([, right]) => ({ status: changedPairs.has(right.split("/").at(-1)) ? "different" : "same", message: null })),
    expand: ({ path, deep }) => view(find(path), !!deep),
    refresh_paths: ({ paths }) => paths.map((p) => (find(p) ? entry(find(p)) : null)),
    delete_entries: ({ paths, left }) => ({ nodes: paths.map((p) => remove(p, left)), error: null, notTrashed: [] }),
    open_file_diff: () => ({
      id: 2,
      rows: fileRows.length,
      sections: [{ start: 1, end: 2 }],
      rowRuns: [{ start: 0, end: 1, kind: "equal" }, { start: 1, end: 2, kind: "changed" }],
      stats: { changed: 1, added: 0, removed: 0, minor: 0 },
      left: fileSide,
      right: fileSide,
      syntax: null,
      identical: false,
      view: "text",
      views: [{ id: "text", label: "Text" }],
      image: null,
      note: null,
      equalView: null,
    }),
    file_rows: ({ start, count }) => fileRows.slice(start, start + count),
    search_diff: ({ query, options: { side } }) => {
      const hits = query === "needle" && side !== "left" ? [{ row: 1, side: 1, start: 0, end: 6 }] : [];
      const result = { hits, truncated: false };
      if (side === "both") return result;
      return new Promise((resolve) => pendingSearches.set(side, () => resolve(result)));
    },
  };

  let callbacks = 0;
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
    transformCallback(callback) {
      const id = ++callbacks;
      window[`_${id}`] = callback;
      return id;
    },
    unregisterCallback() {},
    convertFileSrc: (path) => path,
    async invoke(command, args = {}) {
      calls.push({ command, args });
      if (command in commands) return structuredClone(await commands[command](args));
      if (command === "plugin:event|listen") {
        listeners.set(args.event, [...(listeners.get(args.event) ?? []), args.handler]);
        return ++callbacks;
      }
      return null;
    },
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
  window.__backend = {
    calls,
    count: (name) => calls.filter((c) => c.command === name).length,
    holdRefresh() { holdRefresh = true; },
    editMovedFile(name) {
      changedPairs.add(name);
      for (const handler of listeners.get("paths-changed") ?? []) {
        window[`_${handler}`]({ event: "paths-changed", payload: { session: 1, paths: [`after/${name}`] } });
      }
    },
    async finishSearch(side) {
      const finish = pendingSearches.get(side);
      if (!finish) throw new Error(`No pending ${side} search`);
      pendingSearches.delete(side);
      finish();
      // Let the app handle this response before the test delivers another one.
      await new Promise(requestAnimationFrame);
    },
  };
})();
