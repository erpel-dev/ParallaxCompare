// Paths of recently compared sides, newest first, kept in this browser profile.

const KEY = "parallax.recentPaths";
const MAX = 30;

export function recentPaths(): string[] {
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(saved) ? saved.filter((p) => typeof p === "string") : [];
  } catch {
    return [];
  }
}

export function rememberPaths(...paths: string[]) {
  const fresh = paths.map((p) => p.trim()).filter(Boolean);
  if (!fresh.length) return;
  const list = [...fresh, ...recentPaths().filter((p) => !fresh.includes(p))].slice(0, MAX);
  try {
    localStorage.setItem(KEY, JSON.stringify(list));
  } catch {
    // Remembering paths is a convenience only.
  }
}
