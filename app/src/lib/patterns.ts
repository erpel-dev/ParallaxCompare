/** Split a pattern list without splitting glob alternatives, classes or quoted names. */
export function splitPatterns(text: string, excludes = false): string[] {
  const out: string[] = [];
  let token = "", end = 0, started = false, braces = 0, bracket = false, quote = "";
  const separator = (c: string) => excludes ? c === "," : /[\s,;]/u.test(c);
  const append = (c: string, literal: boolean) => {
    if (!started && !literal && !c.trim()) return;
    token += c;
    started = true;
    if (literal || c.trim()) end = token.length;
  };
  const emit = () => { if (end) out.push(token.slice(0, end)); token = ""; end = 0; started = false; };
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === "\\" && i + 1 < text.length) {
      const next = text[++i];
      // Escaped list separators are literal names, but an escaped comma inside
      // a glob alternative belongs to the glob compiler (also in quoted globs).
      append((!!quote && next === quote) || (!bracket && !braces && separator(next)) ? next : c + next, true);
    } else if (quote && c === quote) {
      quote = "";
    } else if (!quote && !bracket && !started && (c === '"' || c === "'")) {
      quote = c;
      started = true;
    } else if (c === "[" && !bracket) { bracket = true; append(c, true); }
    else if (c === "]" && bracket) { bracket = false; append(c, true); }
    else if (!bracket && c === "{") { braces++; append(c, true); }
    else if (!bracket && c === "}") { braces = Math.max(0, braces - 1); append(c, true); }
    else if (!quote && !bracket && !braces && separator(c)) emit();
    else append(c, !!quote || bracket || braces > 0);
  }
  if (quote) throw new Error("Close the quote around the file pattern.");
  emit();
  return out;
}

/** Quote saved patterns so reopening a view preserves literal separators. */
export function joinPatterns(patterns: string[], excludes = false): string {
  return patterns.map((p) => '"' + p.replaceAll('"', '\\"') + '"').join(excludes ? ", " : " ");
}
