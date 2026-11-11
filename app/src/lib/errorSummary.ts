import type { Node } from "./api";

/** Includes failures of the compared root, whose path is shown as ".". */
function* failures(root: Node): Generator<{ node: Node; path: string }> {
  const pending = [{ node: root, path: "" }];
  while (pending.length) {
    const { node, path } = pending.pop()!;
    if (node.status === "error") yield { node, path: path || "." };
    const children = node.children ?? [];
    for (let i = children.length - 1; i >= 0; i--) {
      const child = children[i];
      pending.push({ node: child, path: path ? `${path}/${child.name}` : child.name });
    }
  }
}

export function causeOf(message: string | undefined): string {
  const text = (message ?? "").replace(/\s*\(os error \d+\)/g, "").trim();
  // Resource diagnostics include a temporary-directory field after the cause.
  // That field must not replace the reason shown in the banner.
  if (/temporary drive is full/i.test(text)) return "temporary drive is full";
  if (/could not use temporary storage/i.test(text)) return "could not use temporary storage";
  if (/not enough memory/i.test(text)) return "not enough memory";
  const separator = text.lastIndexOf(": ");
  const last = (separator < 0 ? text : text.slice(separator + 2)).trim();
  return last ? last.charAt(0).toLowerCase() + last.slice(1) : "could not be read";
}

/** No export strings are built during rendering. Normalize repeated messages once. */
export function summarizeErrors(root: Node) {
  const causes = new Map<string, number>();
  const normalized = new Map<string | undefined, string>();
  let count = 0;
  for (const { node } of failures(root)) {
    let cause = normalized.get(node.message);
    if (cause === undefined) normalized.set(node.message, (cause = causeOf(node.message)));
    causes.set(cause, (causes.get(cause) ?? 0) + 1);
    count++;
  }
  if (!count) return null;
  const top = [...causes].sort((a, b) => b[1] - a[1]).slice(0, 3);
  return { count, top, more: causes.size - top.length };
}

/** Explicitly bounded clipboard export, independent of the true error count. */
export function exportErrors(root: Node, maxLines = 20000, maxChars = 4 * 1024 * 1024) {
  const lines: string[] = [];
  let chars = 0;
  let total = 0;
  let truncated = false;
  for (const { node, path } of failures(root)) {
    total++;
    if (truncated) continue;
    const line = `${path}\t${node.message ?? ""}`;
    if (lines.length >= maxLines || chars + line.length + 1 > maxChars) {
      truncated = true;
      continue;
    }
    lines.push(line);
    chars += line.length + 1;
  }
  const copied = lines.length;
  if (truncated) lines.push(`[List truncated: ${copied.toLocaleString()} of ${total.toLocaleString()} errors copied.]`);
  return { text: lines.join("\n"), copied, total, truncated };
}
