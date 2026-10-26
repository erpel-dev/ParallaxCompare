// Versions of a side in a Git repository (working tree, staged, branches …) and
// switching a side to one of them, for the version chip and the version buttons.
import { api, sourceText, type SourceSpec, type Version, type Versions } from "./api";
import type { SideEdit } from "./PathBar.svelte";
import { notify } from "./toast.svelte";

/** Recent answers, so the chip and the buttons of one side ask once. */
const cache = new Map<string, { at: number; versions: Promise<Versions | null> }>();

/** The versions of `spec`, or `null` when it is not in a local Git repository. */
export function loadVersions(spec: SourceSpec, fresh = false): Promise<Versions | null> {
  const key = JSON.stringify(spec);
  const hit = cache.get(key);
  if (hit && !fresh && Date.now() - hit.at < 5000) return hit.versions;
  const versions = api.sideVersions(spec).catch(() => null);
  cache.set(key, { at: Date.now(), versions });
  if (cache.size > 50) cache.delete(cache.keys().next().value!);
  return versions;
}

export function sameVersion(a: Version, b: Version): boolean {
  return a.type === b.type && (a.type !== "rev" || a.rev === (b as { rev: string }).rev);
}

/** "working tree", "staged", or the revision as written. */
export function versionLabel(v: Version): string {
  return v.type === "worktree" ? "working tree" : v.type === "staged" ? "staged" : v.rev;
}

/** The revision a version starts from, for a merge base: the working tree and the staged files are HEAD plus changes. */
export function revOf(v: Version): string {
  return v.type === "rev" ? v.rev : "HEAD";
}

/** "40 min ago". */
export function ago(seconds: number | null): string {
  if (!seconds) return "";
  const s = Math.max(0, Date.now() / 1000 - seconds);
  if (s < 90) return "just now";
  if (s < 5400) return `${Math.round(s / 60)} min ago`;
  if (s < 36 * 3600) return `${Math.round(s / 3600)} h ago`;
  if (s < 60 * 86400) return `${Math.round(s / 86400)} days ago`;
  return `${Math.round(s / (30 * 86400))} months ago`;
}

/**
 * Switches the side of `edit` to `version`; with `both`, the other side as well
 * (it must be in a repository too). Resolves to whether the comparison changed.
 */
export async function switchVersion(edit: SideEdit, version: Version, both = false): Promise<boolean> {
  try {
    const spec = await edit.spec();
    const next = await api.versionSpec(spec, version);
    if (!both || !edit.other || !edit.applyBoth) {
      if (sourceText(next) === sourceText(spec)) return false;
      return await edit.apply(next);
    }
    const other = await api.versionSpec(await edit.other(), version);
    const [left, right] = edit.side === "right" ? [other, next] : [next, other];
    return await edit.applyBoth(left, right);
  } catch (e) {
    notify(String(e));
    return false;
  }
}
