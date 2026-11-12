/** Selected paths in display order, with descendants of selected entries pruned. */
export function selectionTargets(paths: string[]): string[] {
  const selected = new Set(paths);
  return paths.filter((path) => {
    if (path && selected.has("")) return false;
    for (let slash = path.indexOf("/"); slash !== -1; slash = path.indexOf("/", slash + 1)) {
      if (selected.has(path.slice(0, slash))) return false;
    }
    return true;
  });
}
