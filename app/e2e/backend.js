// A stand-in for the app's backend, loaded into the page before the web view's
// own code. It keeps two small trees, answers the commands the folder view
// uses the way the app does (identical folders come without their entries),
// and counts what was asked, so tests can tell what the view requested.
(() => {
  const file = (size = 4) => ({ kind: "file", size, mtime: 1700000000 });
  const dir = () => ({ kind: "dir", size: 0, mtime: 1700000000 });
  const names = (n, prefix) => Array.from({ length: n }, (_, i) => `${prefix}${String(i).padStart(2, "0")}.txt`);
  const pair = (name, status = "same") => ({ name, left: file(), right: file(status === "same" ? 4 : 9), status });
  const folder = (name, children) => ({ name, left: dir(), right: dir(), status: "same", children });

  // equal/: identical, with a folder inside. mixed/: one changed file, one only
  // on the left, an identical folder. odd/: names with a tab and a line break.
  const root = folder("", [
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

  const calls = [];
  const commands = {
    app_info: () => ({ version: "0.0.0", revision: "test", os: "linux", arch: "x86_64", tempDir: "/tmp" }),
    startup_request: () => ({ kind: "compare", left: { type: "local", path: "/work/left" }, right: { type: "local", path: "/work/right" }, mergeBase: false }),
    open_session: () => ({ id: 1, left: source("/work/left"), right: source("/work/right"), isFile: false }),
    close_session: () => null,
    side_source: ({ left, path }) => ({ type: "local", path: `/work/${left ? "left" : "right"}/${path}` }),
    run_compare: () => ({ root: view(root, false), summary: summary() }),
    expand: ({ path, deep }) => view(find(path), !!deep),
    refresh_paths: ({ paths }) => paths.map((p) => (find(p) ? entry(find(p)) : null)),
    delete_entries: ({ paths, left }) => ({ nodes: paths.map((p) => remove(p, left)), error: null, notTrashed: [] }),
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
      if (command in commands) return structuredClone(commands[command](args));
      // Window, event and other plugin calls, and commands these tests do not need.
      return command.startsWith("plugin:event|listen") ? ++callbacks : null;
    },
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
  window.__backend = { calls, count: (name) => calls.filter((c) => c.command === name).length };
})();
