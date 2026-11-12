/**
 * Markdown of pull request comments, as safe HTML. Comment text comes from
 * anyone who can comment, so nothing of it is taken as HTML: every character
 * is escaped first, and only these pieces become markup: paragraphs, headings,
 * lists, quotes, fenced code (``` and ```suggestion), inline code, bold,
 * italics, strike-through and links. Links are https only and open through
 * the app on click; images are not loaded, only named with their link.
 */

export function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
}

/** The lines a ```suggestion block puts in place of the commented ones. */
export interface Suggestion {
  lines: string[];
}

export interface Rendered {
  html: string;
  suggestions: Suggestion[];
}

const safeUrl = (raw: string): string | null => {
  try {
    const u = new URL(raw);
    return u.protocol === "https:" && !u.username && !u.password ? u.href : null;
  } catch {
    return null;
  }
};

/** Inline markup of one escaped-free text; returns HTML. */
function inline(text: string): string {
  // Code spans first: their content is shown as it is.
  const parts = text.split(/(`+[^`]*`+)/g);
  return parts
    .map((part) => {
      const code = /^(`+)([^`]*)\1$/.exec(part);
      if (code) return `<code>${escapeHtml(code[2])}</code>`;
      let out = "";
      let last = 0;
      // Images and links: ![alt](url) and [text](url), and bare https URLs.
      const re = /(!?)\[([^\]]*)\]\((\S+?)\)|(https:\/\/[^\s<>()]+[^\s<>().,;:!?'"])/g;
      for (let m: RegExpExecArray | null; (m = re.exec(part)); ) {
        out += emphasis(escapeHtml(part.slice(last, m.index)));
        last = m.index + m[0].length;
        const url = safeUrl(m[3] ?? m[4] ?? "");
        const label = m[4] ? m[4] : m[2] || m[3];
        if (m[1] === "!") {
          // No image is loaded from a comment: that could tell the sender the comment was read.
          out += url ? `<a class="md-link" data-href="${escapeHtml(url)}" title="${escapeHtml(url)}">[image: ${escapeHtml(m[2] || "image")}]</a>` : `[image: ${escapeHtml(m[2] || "image")}]`;
        } else if (url) {
          out += `<a class="md-link" data-href="${escapeHtml(url)}" title="${escapeHtml(url)}">${emphasis(escapeHtml(label))}</a>`;
        } else {
          out += escapeHtml(m[0]);
        }
      }
      return out + emphasis(escapeHtml(part.slice(last)));
    })
    .join("");
}

/** Bold, italics and strike-through on already escaped text. */
function emphasis(s: string): string {
  return s
    .replace(/\*\*(?=\S)([^*]*?\S)\*\*/g, "<strong>$1</strong>")
    .replace(/__(?=\S)([^_]*?\S)__/g, "<strong>$1</strong>")
    .replace(/(^|[^\w*])\*(?=\S)([^*]*?\S)\*(?!\w)/g, "$1<em>$2</em>")
    .replace(/(^|[^\w])_(?=\S)([^_]*?\S)_(?!\w)/g, "$1<em>$2</em>")
    .replace(/~~(?=\S)([^~]*?\S)~~/g, "<del>$1</del>");
}

export function renderMarkdown(source: string): Rendered {
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const out: string[] = [];
  const suggestions: Suggestion[] = [];
  let para: string[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;
  let quote: string[] = [];
  const flushPara = () => {
    if (para.length) out.push(`<p>${inline(para.join(" "))}</p>`);
    para = [];
  };
  const flushList = () => {
    if (list) out.push(`<${list.ordered ? "ol" : "ul"}>${list.items.map((i) => `<li>${inline(i)}</li>`).join("")}</${list.ordered ? "ol" : "ul"}>`);
    list = null;
  };
  const flushQuote = () => {
    if (quote.length) out.push(`<blockquote>${renderMarkdown(quote.join("\n")).html}</blockquote>`);
    quote = [];
  };
  const flush = () => {
    flushPara();
    flushList();
    flushQuote();
  };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fence = /^\s*(```+|~~~+)\s*([\w+-]*)/.exec(line);
    if (fence) {
      flush();
      const body: string[] = [];
      for (i++; i < lines.length && !lines[i].trim().startsWith(fence[1]); i++) body.push(lines[i]);
      if (fence[2] === "suggestion") {
        suggestions.push({ lines: body });
        out.push(`<div class="md-suggestion" data-index="${suggestions.length - 1}"></div>`);
      } else {
        out.push(`<pre><code>${escapeHtml(body.join("\n"))}</code></pre>`);
      }
      continue;
    }
    if (/^\s*>/.test(line)) {
      flushPara();
      flushList();
      quote.push(line.replace(/^\s*>\s?/, ""));
      continue;
    }
    flushQuote();
    if (!line.trim()) {
      flushPara();
      flushList();
      continue;
    }
    const heading = /^\s*(#{1,6})\s+(.*)$/.exec(line);
    if (heading) {
      flush();
      out.push(`<p class="md-h">${inline(heading[2])}</p>`);
      continue;
    }
    const item = /^\s*([-*+]|\d+[.)])\s+(.*)$/.exec(line);
    if (item) {
      flushPara();
      const ordered = /\d/.test(item[1]);
      if (!list || list.ordered !== ordered) {
        flushList();
        list = { ordered, items: [] };
      }
      list.items.push(item[2].replace(/^\[( |x)\]\s*/i, (_m, x) => (x.trim() ? "☑ " : "☐ ")));
      continue;
    }
    if (list && /^\s{2,}\S/.test(line)) {
      list.items[list.items.length - 1] += " " + line.trim();
      continue;
    }
    if (/^\s*(-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {
      flush();
      out.push("<hr>");
      continue;
    }
    // Raw HTML stays text: <details> and comments are shown as written, escaped.
    para.push(line.trim());
  }
  flush();
  return { html: out.join(""), suggestions };
}
