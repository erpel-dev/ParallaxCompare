// Actions the command palette lists. Views and the app each supply theirs.

export interface Command {
  /** What it does, e.g. "Expand all folders". */
  title: string;
  /** Where it belongs, shown before the title: "Folder", "Text", "Left side" … */
  group: string;
  /** The shortcut, for display. */
  keys?: string;
  /** False when it does not apply right now; the palette shows it dimmed. */
  enabled?: boolean;
  run: () => void;
}

/** How well `query` matches `text`: lower is better, `null` for no match. */
export function matchScore(text: string, query: string): number | null {
  const t = text.toLowerCase();
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  let score = 0;
  for (const word of words) {
    const at = t.indexOf(word);
    if (at >= 0) {
      // Matches at a word start beat matches inside a word.
      score += at === 0 || /[\s:/(]/.test(t[at - 1]) ? 0 : 1;
      continue;
    }
    // Letters in order, e.g. "expall" for "Expand all".
    let i = 0;
    for (const ch of t) if (ch === word[i]) i++;
    if (i < word.length) return null;
    score += 3;
  }
  return score;
}
