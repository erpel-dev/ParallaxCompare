/** Split a pattern list without splitting glob alternatives, classes or quoted names. */
export function splitPatterns(text: string, excludes = false): string[] {
  const out: string[] = [];
  let token = "", braces = 0, bracket = false, quote = "";
  const separator = (c: string) => excludes ? c === "," : /[\s,;]/u.test(c);
  const emit = () => { if (token.trim()) out.push(token.trim()); token = ""; };
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === "\\" && i + 1 < text.length) {
      const next = text[++i];
      token += !bracket && (separator(next) || next === quote) ? next : c + next;
    } else if (quote) {
      if (c === quote) quote = "";
      else token += c;
    } else if (!bracket && !token.trim() && (c === '"' || c === "'")) {
      quote = c;
    } else if (c === "[" && !bracket) { bracket = true; token += c; }
    else if (c === "]" && bracket) { bracket = false; token += c; }
    else if (!bracket && c === "{") { braces++; token += c; }
    else if (!bracket && c === "}") { braces = Math.max(0, braces - 1); token += c; }
    else if (!bracket && !braces && separator(c)) emit();
    else token += c;
  }
  if (quote) throw new Error("Close the quote around the file pattern.");
  emit();
  return out;
}

/** Quote saved patterns so reopening a view preserves literal separators. */
export function joinPatterns(patterns: string[], excludes = false): string {
  return patterns.map((p) => '"' + p.replaceAll('"', '\\"') + '"').join(excludes ? ", " : " ");
}
