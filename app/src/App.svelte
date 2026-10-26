<script lang="ts">
  import { onMount, tick } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { ask } from "@tauri-apps/plugin-dialog";
  import ConflictsView from "./lib/ConflictsView.svelte";
  import FolderView from "./lib/FolderView.svelte";
  import MergeView from "./lib/MergeView.svelte";
  import NewCompare from "./lib/NewCompare.svelte";
  import type { SideEdit } from "./lib/PathBar.svelte";
  import TextView from "./lib/TextView.svelte";
  import { api, isPullRequestUrl, sourceName, sourceText, type MergeRequest, type PullRequestInfo, type SessionInfo, type SourceOptions, type SourceSpec } from "./lib/api";
  import type { NavActions } from "./lib/Nav.svelte";
  import type { Command } from "./lib/commands";
  import Palette from "./lib/Palette.svelte";
  import { recentComparisons, rememberComparison, rememberPaths } from "./lib/recent";
  import { clearErrors, notify } from "./lib/toast.svelte";
  import Toasts from "./lib/Toasts.svelte";
  import type { RowSide } from "./lib/tree";
  import type { ViewState } from "./lib/viewState";
  import { baseName } from "./lib/format";

  interface Request {
    left: SourceSpec;
    right: SourceSpec;
    options: SourceOptions;
    mergeBase: boolean;
    pullRequest?: PullRequestInfo;
  }

  /** What every comparison tab keeps besides its session. */
  interface Shared {
    /** Earlier and later comparisons shown in this tab, for back and forward. */
    back?: Request[];
    forward?: Request[];
    /** Both sides one folder up; `null` when either side is at the top. */
    up?: Request | null;
    /** A row for the folder view to select and scroll to; `n` makes each request new. */
    reveal?: { path: string; n: number };
    /** How the comparison this one replaced was shown, after a side was switched. */
    restore?: ViewState;
  }

  type FolderTab = { key: number; kind: "folder"; title: string; session: SessionInfo; request: Request } & Shared;
  type TextTab = {
    key: number;
    kind: "text";
    title: string;
    session: SessionInfo;
    path: string;
    /** Set for a comparison of two files; a file opened from a folder comparison shares that session. */
    request?: Request;
    dirty?: boolean;
  } & Shared;
  type MergeTab = { key: number; kind: "merge"; title: string; request: MergeRequest; dirty?: boolean };
  type ConflictsTab = { key: number; kind: "conflicts"; title: string; dir: string };
  type Tab = { key: number; kind: "new"; title: string } | FolderTab | TextTab | MergeTab | ConflictsTab;
  type SessionTab = FolderTab | TextTab;
  const isSession = (t: Tab | undefined): t is SessionTab => t?.kind === "folder" || t?.kind === "text";
  const icons: Record<Tab["kind"], string> = { new: "＋", folder: "📁", text: "📄", merge: "⑂", conflicts: "⚠" };
  type SideName = "left" | "right";

  let tabs = $state<Tab[]>([]);
  let active = $state(0);
  // The pages stay in the order they were opened, so reordering tabs never moves
  // (and resets) an open view.
  const pages = $derived([...tabs].sort((a, b) => a.key - b.key));

  // ---- Reordering tabs by dragging ----
  let drag: { key: number; x: number; moved: boolean } | null = null;
  let dragging = $state<number | null>(null);
  let justDragged = false;

  function tabDown(e: PointerEvent, key: number) {
    if (e.button !== 0 || (e.target as HTMLElement).closest(".x")) return;
    drag = { key, x: e.clientX, moved: false };
  }

  function tabMove(e: PointerEvent) {
    if (!drag || !tabBar) return;
    if (!drag.moved) {
      if (Math.abs(e.clientX - drag.x) < 6) return;
      // Captured only once it is a drag, so that a plain click still reaches the tab's button.
      (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
      drag.moved = true;
    }
    dragging = active = drag.key;
    // Where the tab goes: before the first tab whose middle is right of the pointer.
    const els = [...tabBar.querySelectorAll<HTMLElement>(".tab")];
    const from = tabs.findIndex((t) => t.key === drag!.key);
    let to = els.findIndex((el) => {
      const r = el.getBoundingClientRect();
      return e.clientX < r.left + r.width / 2;
    });
    to = to === -1 ? tabs.length - 1 : to > from ? to - 1 : to;
    if (from !== -1 && to !== from) {
      const [tab] = tabs.splice(from, 1);
      tabs.splice(to, 0, tab);
    }
    // Near an edge of a bar that scrolls, it scrolls along.
    const bar = tabBar.getBoundingClientRect();
    if (e.clientX < bar.left + 30) tabBar.scrollLeft -= 12;
    else if (e.clientX > bar.right - 30) tabBar.scrollLeft += 12;
  }

  /** Moves the active tab one place to the left or right. */
  function moveTab(dir: -1 | 1) {
    const from = tabs.findIndex((t) => t.key === active);
    const to = from + dir;
    if (from === -1 || to < 0 || to >= tabs.length) return;
    const [tab] = tabs.splice(from, 1);
    tabs.splice(to, 0, tab);
  }

  function tabUp() {
    // The click that ends a drag does not count as one.
    if (drag?.moved) {
      justDragged = true;
      setTimeout(() => (justDragged = false));
    }
    drag = null;
    dragging = null;
  }
  let nextKey = 1;
  let revealCount = 0;
  let paletteOpen = $state(false);
  /** The views of the comparison tabs, for their commands. */
  let views: Record<number, { commands(): Command[]; viewState?(): ViewState["state"] }> = $state({});
  /** The new-comparison forms, by tab, to fill in dropped paths. */
  let pickers: Record<number, NewCompare> = $state({});
  /** Files being dragged over the window, and the side they would replace. */
  let drop = $state<{ side: SideName | "both"; count: number } | null>(null);
  let mainEl = $state<HTMLElement>();
  let tabBar = $state<HTMLElement>();
  // The active tab is scrolled into view when the bar is too narrow for all tabs.
  $effect(() => {
    const key = active;
    tick().then(() => tabBar?.querySelector(`[data-key="${CSS.escape(String(key))}"]`)?.scrollIntoView({ block: "nearest", inline: "nearest" }));
  });

  function add(tab: Tab) {
    tabs.push(tab);
    active = tab.key;
  }

  function newTab() {
    add({ key: nextKey++, kind: "new", title: "New comparison" });
  }

  function titleFor(session: SessionInfo): string {
    const side = (s: SessionInfo["left"]) => (s.rev ? s.rev : baseName(s.display));
    return `${side(session.left)} ↔ ${side(session.right)}`;
  }

  function describe(r: Pick<Request, "left" | "right">): string {
    return `${sourceName(r.left)} ↔ ${sourceName(r.right)}`;
  }

  function sessionTab(session: SessionInfo, request: Request, shared: Shared = {}): SessionTab {
    const key = nextKey++;
    if (request.pullRequest) {
      session.left.rev = "Merge base";
      session.right.rev = `PR #${request.pullRequest.number} head`;
    }
    const title = request.pullRequest ? `PR #${request.pullRequest.number}: ${request.pullRequest.title}` : titleFor(session);
    const tab: SessionTab = session.isFile
      ? { key, kind: "text", title, session, path: "", request, ...shared }
      : { key, kind: "folder", title, session, request, ...shared };
    findUp(key, request);
    return tab;
  }

  /** Works out what "one level up" compares, once the tab exists. */
  async function findUp(key: number, request: Request) {
    const [left, right] = await Promise.all([api.sourceParent(request.left), api.sourceParent(request.right)]).catch(
      () => [null, null],
    );
    const tab = tabs.find((t) => t.key === key);
    if (isSession(tab)) tab.up = left && right ? { ...request, left, right } : null;
  }

  function remember(request: Request) {
    rememberPaths(sourceText(request.left), sourceText(request.right));
    if (request.pullRequest) rememberPaths(request.pullRequest.url);
    rememberComparison($state.snapshot(request));
    clearErrors();
  }

  async function start(request: Request, replaceKey?: number, inlineError = false) {
    try {
      const session = await api.openSession(request.left, request.right, request.options, request.mergeBase);
      const tab = sessionTab(session, request);
      const index = tabs.findIndex((t) => t.key === replaceKey);
      if (index >= 0) tabs[index] = tab;
      else tabs.push(tab);
      active = tab.key;
      remember(request);
    } catch (e) {
      if (inlineError) throw e;
      notify(String(e));
    }
  }

  /** Releases what a comparison tab holds: its session, and the file tabs that share it. */
  function release(tab: SessionTab) {
    if (tab.kind === "folder") {
      api.closeSession(tab.session.id);
      tabs = tabs.filter((t) => !(t.kind === "text" && !t.request && t.session.id === tab.session.id));
    } else if (tab.request) {
      api.closeSession(tab.session.id);
    }
  }

  /** The folder comparison a file was opened from. */
  function parentOf(tab: SessionTab): FolderTab | undefined {
    if (tab.kind !== "text" || tab.request) return undefined;
    return tabs.find((t): t is FolderTab => t.kind === "folder" && t.session.id === tab.session.id);
  }

  /**
   * What `tab` compares, with some sides replaced. For a file opened from a folder
   * comparison, its sides are that file on each side.
   */
  async function requestOf(tab: SessionTab, changes: Partial<Record<SideName, SourceSpec>> = {}): Promise<Request> {
    if (tab.request) return { ...tab.request, ...changes, pullRequest: Object.keys(changes).length ? undefined : tab.request.pullRequest };
    const side = (s: SideName) => (changes[s] ? Promise.resolve(changes[s]) : sideSpec(tab, s));
    const [left, right] = await Promise.all([side("left"), side("right")]);
    const options = parentOf(tab)?.request.options ?? { expandArchives: true, ignored: "mark" };
    return { left, right, options, mergeBase: false };
  }

  /**
   * Compares `request` in place of what `tab` shows. The old comparison stays
   * until the new one has opened, so a mistyped path costs nothing. `how` says
   * how the tab's back/forward history moves; `reveal` names a row to select.
   */
  async function replace(
    tab: SessionTab,
    request: Request,
    how: "new" | "back" | "forward" = "new",
    reveal?: string,
    restore?: ViewState,
  ): Promise<boolean> {
    const current = await requestOf(tab).catch(() => null);
    if (!(await confirmDiscard(unsaved(tab)))) return false;
    let session: SessionInfo;
    try {
      session = await api.openSession(request.left, request.right, request.options, request.mergeBase);
    } catch (e) {
      notify(String(e));
      return false;
    }
    if (!tabs.some((t) => t.key === tab.key)) {
      api.closeSession(session.id);
      return false;
    }
    release(tab);
    const [back, forward] = [tab.back ?? [], tab.forward ?? []];
    const keep = (list: Request[]) => (current ? [...list, current].slice(-50) : list);
    const history =
      how === "back"
        ? { back: back.slice(0, -1), forward: keep(forward) }
        : how === "forward"
          ? { back: keep(back), forward: forward.slice(0, -1) }
          : { back: keep(back), forward: [] };
    const next = sessionTab(session, request, {
      ...history,
      reveal: reveal ? { path: reveal, n: ++revealCount } : undefined,
      // Only a view of the same kind can take it over.
      restore: restore && restore.kind === (session.isFile ? "text" : "folder") ? restore : undefined,
    });
    tabs[tabs.findIndex((t) => t.key === tab.key)] = next;
    active = next.key;
    remember(request);
    return true;
  }

  /** Compares `tab` with some sides replaced; a file from a folder comparison becomes a comparison of its own. */
  async function changeSides(tab: SessionTab, changes: Partial<Record<SideName, SourceSpec>>): Promise<boolean> {
    let request: Request;
    try {
      request = await requestOf(tab, changes);
    } catch (e) {
      notify(String(e));
      return false;
    }
    // The view stays as it was: open folders, selection, scroll position.
    const state = views[tab.key]?.viewState?.();
    const restore = state && ((tab.kind === "folder" ? { kind: "folder", state } : { kind: "text", state }) as ViewState);
    return replace(tab, request, "new", undefined, restore);
  }

  function sideSpec(tab: SessionTab, side: SideName): Promise<SourceSpec> {
    if (tab.request) return Promise.resolve(tab.request[side]);
    return api.sideSource(tab.session.id, side === "left", tab.kind === "text" ? tab.path : "");
  }

  function sideEdits(tab: SessionTab): [SideEdit, SideEdit] {
    const openPullRequest = async (url: string): Promise<boolean> => {
      try {
        const pr = await api.resolvePullRequest(url);
        const { title, number } = pr;
        return replace(tab, {
          left: pr.left, right: pr.right,
          options: tab.request?.options ?? parentOf(tab)?.request.options ?? { expandArchives: true, ignored: "mark" },
          mergeBase: false, pullRequest: { url: pr.url, title, number },
        });
      } catch (e) {
        notify(String(e));
        return false;
      }
    };
    const make = (side: SideName): SideEdit => ({
      spec: () => sideSpec(tab, side),
      apply: (spec) => {
        const text = spec.type === "local" ? spec.path : spec.repo;
        return isPullRequestUrl(text) ? openPullRequest(text) : changeSides(tab, { [side]: spec });
      },
      openPullRequest,
      file: tab.kind === "text",
      side,
      other: () => sideSpec(tab, side === "left" ? "right" : "left"),
      applyBoth: (left, right) => changeSides(tab, { left, right }),
    });
    return [make("left"), make("right")];
  }

  async function swap(tab: SessionTab) {
    try {
      const r = await requestOf(tab);
      await replace(tab, { ...r, left: r.right, right: r.left, pullRequest: undefined });
    } catch (e) {
      notify(String(e));
    }
  }

  /** One level up: from a file back to its folder comparison, otherwise both sides' parent folders. */
  function goUp(tab: SessionTab) {
    const parent = parentOf(tab);
    if (parent && tab.kind === "text") {
      parent.reveal = { path: tab.path, n: ++revealCount };
      active = parent.key;
    } else if (tab.up) {
      replace(tab, tab.up, "new", tab.request ? sourceName(tab.request.left) : undefined);
    }
  }

  function navFor(tab: SessionTab): NavActions {
    const back = tab.back?.at(-1);
    const forward = tab.forward?.at(-1);
    const parent = parentOf(tab);
    return {
      back: back ? () => replace(tab, back, "back") : null,
      backTitle: back ? `Back to ${describe(back)}` : "Back",
      forward: forward ? () => replace(tab, forward, "forward") : null,
      forwardTitle: forward ? `Forward to ${describe(forward)}` : "Forward",
      up: parent || tab.up ? () => goUp(tab) : null,
      upTitle: parent
        ? "Back to the folder comparison"
        : tab.up
          ? `One level up: ${describe(tab.up)}`
          : "Both sides are at the top",
    };
  }

  /** Compares two rows of a folder comparison with each other, in a new tab. */
  async function compareRows(tab: FolderTab, a: RowSide, b: RowSide) {
    try {
      const [left, right] = await Promise.all([
        api.sideSource(tab.session.id, a.left, a.path),
        api.sideSource(tab.session.id, b.left, b.path),
      ]);
      await start({ left, right, options: tab.request.options, mergeBase: false });
    } catch (e) {
      notify(String(e));
    }
  }

  /** Opens a 3-way merge in a new tab, or shows the one already open for that file. */
  function openMerge(request: MergeRequest, title: string, replaceKey?: number) {
    const same = (t: Tab) =>
      t.kind === "merge" && request.kind === "repo" && t.request.kind === "repo" && t.request.workdir === request.workdir && t.request.path === request.path;
    const existing = tabs.find(same);
    if (existing) {
      active = existing.key;
      return;
    }
    const tab: MergeTab = { key: nextKey++, kind: "merge", title, request };
    const index = tabs.findIndex((t) => t.key === replaceKey);
    if (index >= 0) tabs[index] = tab;
    else tabs.push(tab);
    active = tab.key;
    clearErrors();
  }

  function openConflicts(dir: string, replaceKey?: number) {
    const tab: ConflictsTab = { key: nextKey++, kind: "conflicts", title: `Conflicts · ${baseName(dir)}`, dir };
    const index = tabs.findIndex((t) => t.key === replaceKey);
    if (index >= 0) tabs[index] = tab;
    else tabs.push(tab);
    active = tab.key;
    rememberPaths(dir);
    clearErrors();
  }

  /** File tabs with unsaved edits among those that closing `tab` closes. */
  function unsaved(tab: Tab): Tab[] {
    if (tab.kind === "text" || tab.kind === "merge") return tab.dirty ? [tab] : [];
    if (tab.kind !== "folder") return [];
    return tabs.filter((t) => t.kind === "text" && t.dirty && !t.request && t.session.id === tab.session.id);
  }

  async function confirmDiscard(dirty: Tab[]): Promise<boolean> {
    if (!dirty.length) return true;
    const names = dirty.map((t) => t.title).join(", ");
    return ask(`Discard the unsaved changes in ${names}?`, { title: "Unsaved changes", kind: "warning", okLabel: "Discard", cancelLabel: "Keep editing" });
  }

  async function close(key: number) {
    const index = tabs.findIndex((t) => t.key === key);
    if (index < 0) return;
    const tab = tabs[index];
    if (!(await confirmDiscard(unsaved(tab)))) return;
    if (isSession(tab)) release(tab);
    tabs = tabs.filter((t) => t.key !== key);
    if (active === key) active = tabs[Math.min(index, tabs.length - 1)]?.key ?? 0;
    if (!tabs.length) newTab();
  }

  function openFile(session: SessionInfo, path: string) {
    const existing = tabs.find((t) => t.kind === "text" && t.session.id === session.id && t.path === path);
    if (existing) {
      active = existing.key;
      return;
    }
    add({ key: nextKey++, kind: "text", title: baseName(path), session, path });
  }

  /** The mouse's back and forward buttons move through the active tab's history. */
  function onMouseUp(e: MouseEvent) {
    if (e.button !== 3 && e.button !== 4) return;
    const tab = tabs.find((t) => t.key === active);
    if (!isSession(tab)) return;
    const nav = navFor(tab);
    (e.button === 3 ? nav.back : nav.forward)?.();
    e.preventDefault();
  }

  function dropSide(x: number, count: number): SideName | "both" {
    if (count >= 2) return "both";
    const r = mainEl!.getBoundingClientRect();
    return x < r.left + r.width / 2 ? "left" : "right";
  }

  function dropLabel(d: { side: SideName | "both"; count: number }): string {
    const tab = tabs.find((t) => t.key === active);
    if (d.side === "both") return d.count > 2 ? "Drop two files or folders to compare them" : "Compare these two";
    const side = d.side === "left" ? "left" : "right";
    return tab?.kind === "new" ? `Use as the ${side} side` : `Compare with this on the ${side}`;
  }

  /** Paths dropped on the window: on a comparison they replace one side (or both), on a new tab they fill the form. */
  async function dropped(paths: string[], side: SideName | "both") {
    const tab = tabs.find((t) => t.key === active);
    if (!tab || !paths.length || paths.length > 2) return;
    const spec = (path: string): SourceSpec => ({ type: "local", path });
    if (tab.kind === "new") {
      const form = pickers[tab.key];
      if (side === "both") form?.fill(paths[0], paths[1]);
      else form?.fill(side === "left" ? paths[0] : undefined, side === "right" ? paths[0] : undefined);
      return;
    }
    if (!isSession(tab)) return;
    if (side === "both") await changeSides(tab, { left: spec(paths[0]), right: spec(paths[1]) });
    else await changeSides(tab, { [side]: spec(paths[0]) });
  }

  function cycle(step: number) {
    const index = tabs.findIndex((t) => t.key === active);
    active = tabs[(index + step + tabs.length) % tabs.length].key;
  }

  /** The app's own commands, then the current tab's. */
  function appCommands(): Command[] {
    const group = "App";
    const list: Command[] = [
      { group, title: "New comparison", keys: "Ctrl+T", run: newTab },
      { group, title: "Close tab", keys: "Ctrl+W", run: () => close(active) },
      { group, title: "Next tab", keys: "Ctrl+Tab", enabled: tabs.length > 1, run: () => cycle(1) },
      { group, title: "Previous tab", keys: "Ctrl+Shift+Tab", enabled: tabs.length > 1, run: () => cycle(-1) },
    ];
    const tab = tabs.find((t) => t.key === active);
    if (isSession(tab)) {
      const nav = navFor(tab);
      list.push(
        { group: "Navigate", title: nav.back ? nav.backTitle : "Back", keys: "Mouse back", enabled: !!nav.back, run: () => nav.back?.() },
        { group: "Navigate", title: nav.forward ? nav.forwardTitle : "Forward", enabled: !!nav.forward, run: () => nav.forward?.() },
        { group: "Navigate", title: nav.up ? nav.upTitle : "Up one level", enabled: !!nav.up, run: () => nav.up?.() },
      );
    }
    for (const t of tabs) {
      if (t.key !== active) list.push({ group: "Go to tab", title: t.title, run: () => (active = t.key) });
    }
    for (const c of recentComparisons()) {
      list.push({
        group: c.pinned ? "Pinned" : "Recent",
        title: describe(c) === `${sourceText(c.left)} ↔ ${sourceText(c.right)}`
          ? describe(c)
          : `${describe(c)}  ·  ${sourceText(c.left)} ↔ ${sourceText(c.right)}`,
        run: () => start({ left: c.left, right: c.right, options: c.options, mergeBase: c.mergeBase }),
      });
    }
    return list;
  }

  function allCommands(): Command[] {
    return [...(views[active]?.commands() ?? []), ...appCommands()];
  }

  function onKey(e: KeyboardEvent) {
    const mod = e.ctrlKey || e.metaKey;
    const key = e.key.toLowerCase();
    if (mod && key === "w") close(active);
    else if (mod && key === "t") newTab();
    else if ((mod && e.shiftKey && key === "p") || (mod && !e.shiftKey && key === "k") || e.key === "F1") {
      paletteOpen = !paletteOpen;
    } else if (e.ctrlKey && e.key === "Tab") cycle(e.shiftKey ? -1 : 1);
    else if (e.ctrlKey && e.shiftKey && (e.key === "PageUp" || e.key === "PageDown")) moveTab(e.key === "PageUp" ? -1 : 1);
    // Ctrl+1 … Ctrl+8 pick a tab, Ctrl+9 the last.
    else if (mod && !e.shiftKey && !e.altKey && /^[1-9]$/.test(e.key) && tabs.length) {
      const n = Number(e.key);
      active = tabs[n === 9 ? tabs.length - 1 : Math.min(n, tabs.length) - 1].key;
    } else return;
    e.preventDefault();
  }

  onMount(async () => {
    getCurrentWebview().onDragDropEvent((e) => {
      const p = e.payload;
      if (p.type === "leave") {
        drop = null;
        return;
      }
      const x = p.position.x / window.devicePixelRatio;
      if (p.type === "enter") drop = { side: dropSide(x, p.paths.length), count: p.paths.length };
      else if (p.type === "over" && drop) drop = { ...drop, side: dropSide(x, drop.count) };
      else if (p.type === "drop") {
        const side = dropSide(x, p.paths.length);
        drop = null;
        dropped(p.paths, side);
      }
    });
    getCurrentWindow().onCloseRequested(async (e) => {
      if (!(await confirmDiscard(tabs.filter((t) => (t.kind === "text" || t.kind === "merge") && t.dirty)))) e.preventDefault();
    });
    const startup = await api.startupRequest().catch(() => null);
    if (startup?.kind === "compare") {
      await start({
        left: startup.left,
        right: startup.right,
        mergeBase: startup.mergeBase,
        options: { expandArchives: true, ignored: "mark" },
      });
    } else if (startup?.kind === "merge") {
      openMerge({ kind: "startup" }, "Merge");
    } else if (startup?.kind === "conflicts") {
      openConflicts(startup.dir);
    }
    if (!tabs.length) newTab();
  });
</script>

<svelte:window onkeydown={onKey} onmouseup={onMouseUp} />

<div class="app">
  <div
    class="tabs"
    aria-label="Comparisons"
    role="tablist"
    bind:this={tabBar}
    onwheel={(e) => {
      // The wheel scrolls the tabs sideways.
      if (tabBar && e.deltaY && !e.deltaX) {
        tabBar.scrollLeft += e.deltaY;
        e.preventDefault();
      }
    }}
  >
    {#each tabs as tab (tab.key)}
      <div
        class="tab"
        class:on={tab.key === active}
        class:dragging={tab.key === dragging}
        data-key={tab.key}
        role="tab"
        tabindex="-1"
        aria-selected={tab.key === active}
        onpointerdown={(e) => tabDown(e, tab.key)}
        onpointermove={tabMove}
        onpointerup={tabUp}
        onpointercancel={tabUp}
      >
        <button class="label" onclick={() => !justDragged && (active = tab.key)} title={`${tab.title}\nDrag to move the tab`}>
          <span>{icons[tab.kind]}</span><span class="name">{tab.title}</span>{(tab.kind === "text" || tab.kind === "merge") && tab.dirty ? " ●" : ""}
        </button>
        <button class="x" onclick={() => close(tab.key)} title="Close (Ctrl+W)">×</button>
      </div>
    {/each}
    <button class="add" onclick={newTab} title="New comparison (Ctrl+T)">＋</button>
  </div>

  <main bind:this={mainEl}>
    {#each pages as tab (tab.key)}
      <section class="page" hidden={tab.key !== active}>
        {#if tab.kind === "new"}
          <NewCompare
            bind:this={pickers[tab.key]}
            onCompare={(left, right, options, mergeBase, pullRequest) => start({ left, right, options, mergeBase, pullRequest }, tab.key, true)}
            onMerge={(request) => openMerge(request, request.kind === "sources" ? `Merge · ${sourceName(request.local)}` : "Merge", tab.key)}
            onConflicts={(dir) => openConflicts(dir, tab.key)}
          />
        {:else if tab.kind === "merge"}
          <MergeView request={tab.request} onInfo={(info) => (tab.title = `Merge · ${info.title}`)} onDirty={(d) => (tab.dirty = d)} />
        {:else if tab.kind === "conflicts"}
          <ConflictsView
            dir={tab.dir}
            onOpenMerge={(workdir, path) => openMerge({ kind: "repo", workdir, path }, `Merge · ${baseName(path)}`)}
            onLoaded={(state) => (tab.title = `Conflicts · ${baseName(state.workdir)}${state.files.length ? ` (${state.files.length})` : ""}`)}
          />
        {:else if tab.kind === "folder"}
          <FolderView
            bind:this={views[tab.key]}
            session={tab.session}
            onOpenFile={(p) => openFile(tab.session, p)}
            onSwap={() => swap(tab)}
            onCompareRows={(a, b) => compareRows(tab, a, b)}
            edit={sideEdits(tab)}
            onSetBase={(changes) => changeSides(tab, changes)}
            nav={navFor(tab)}
            reveal={tab.reveal}
            restore={tab.restore?.kind === "folder" ? tab.restore.state : undefined}
            onOpenComparison={(left, right) => start({ left, right, options: tab.request.options, mergeBase: false })}
          />
        {:else}
          <TextView
            bind:this={views[tab.key]}
            session={tab.session}
            path={tab.path}
            onDirty={(d) => (tab.dirty = d)}
            onSwap={() => swap(tab)}
            edit={sideEdits(tab)}
            nav={navFor(tab)}
            restore={tab.restore?.kind === "text" ? tab.restore.state : undefined}
          />
        {/if}
      </section>
    {/each}
    {#if drop}
      <div class="drop {drop.side}">
        <span>{dropLabel(drop)}</span>
      </div>
    {/if}
  </main>
</div>

{#if paletteOpen}
  <Palette commands={allCommands()} onClose={() => (paletteOpen = false)} />
{/if}
<Toasts />

<style>
  .app {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .tabs {
    display: flex;
    background: var(--chrome);
    border-bottom: 1px solid var(--line);
    overflow-x: auto;
    flex: none;
  }
  /* Tabs shrink to fit; past that the bar scrolls, without a scrollbar over the titles. */
  .tabs {
    scrollbar-width: none;
  }
  .tabs::-webkit-scrollbar {
    display: none;
  }
  .tab {
    display: flex;
    align-items: center;
    border-right: 1px solid var(--line);
    border-top: 2px solid transparent;
    max-width: 280px;
    min-width: 96px;
    flex: 0 1 auto;
  }
  .tab .label {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
  }
  .label .name {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tab.on {
    background: var(--panel);
    border-top-color: var(--accent);
  }
  .tab.dragging {
    box-shadow: 0 0 0 1px var(--accent) inset;
    cursor: grabbing;
  }
  .tab button {
    background: none;
    border: none;
    border-radius: 0;
  }
  .label {
    display: flex;
    gap: 6px;
    text-align: left;
    padding: 6px 4px 6px 12px;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--dim);
  }
  .tab.on .label {
    color: var(--text);
  }
  .x {
    color: var(--dim);
    padding: 4px 8px;
  }
  .x:hover {
    color: var(--text);
  }
  .add {
    background: none;
    border: none;
    color: var(--dim);
  }
  main {
    flex: 1;
    min-height: 0;
    position: relative;
  }
  .page {
    position: absolute;
    inset: 0;
  }
  .page[hidden] {
    display: none;
  }
  .drop {
    position: absolute;
    top: 0;
    bottom: 0;
    z-index: 50;
    display: flex;
    align-items: center;
    justify-content: center;
    pointer-events: none;
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    border: 2px dashed var(--accent);
    border-radius: 6px;
  }
  .drop.left {
    left: 0;
    right: 50%;
  }
  .drop.right {
    left: 50%;
    right: 0;
  }
  .drop.both {
    left: 0;
    right: 0;
  }
  .drop span {
    padding: 8px 14px;
    border-radius: 6px;
    background: var(--panel);
    border: 1px solid var(--accent);
    font-weight: 600;
  }
</style>
