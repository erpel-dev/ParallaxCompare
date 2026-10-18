//! reStructuredText with its layout normalized, so that re-wrapping a paragraph or
//! lengthening a heading's underline is not a change.
//!
//! Each paragraph and list item becomes one line; heading adornments are made as
//! long as the title; runs of blank lines and trailing spaces go. What layout
//! means in reST stays: literal blocks (after `::`), the content of code, math
//! and raw directives, tables, line blocks and directive options are kept line
//! for line, and indentation is kept, since it nests blocks.

/// Characters docutils accepts for section adornments.
const ADORNMENT: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

/// Directives whose content is taken literally.
const LITERAL_DIRECTIVES: &[&str] = &[
    "code",
    "code-block",
    "sourcecode",
    "literalinclude",
    "math",
    "raw",
    "parsed-literal",
    "csv-table",
    "list-table",
    "graphviz",
    "uml",
    "plantuml",
    "mermaid",
    "needuml",
    "needarch",
    "jinja",
];

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// A heading's over- or underline: one punctuation character repeated. Not
/// `::` alone, which announces a literal block.
fn is_adornment(line: &str) -> bool {
    let line = line.trim_end();
    if line == "::" {
        return false;
    }
    let mut chars = line.chars();
    match chars.next() {
        Some(c) if ADORNMENT.contains(c) => line.len() >= 2 && chars.all(|d| d == c),
        _ => false,
    }
}

/// The width of a list marker at the start of `text`, so continuation lines
/// indented to the item's text join it.
fn list_marker(text: &str) -> Option<usize> {
    let bullet = ["- ", "* ", "+ ", "• ", "‣ ", "⁃ "];
    if let Some(b) = bullet.iter().find(|b| text.starts_with(**b)) {
        return Some(b.len());
    }
    // 1. 1) (1) a. A. #. i.
    let rest = text.trim_start_matches('(');
    let open = text.len() - rest.len();
    let label = rest.find(['.', ')']).filter(|&n| n > 0 && n <= 4)?;
    let (head, tail) = rest.split_at(label);
    let enumerator = head == "#"
        || head.chars().all(|c| c.is_ascii_digit())
        || (head.len() == 1 && head.chars().all(|c| c.is_ascii_alphabetic()))
        || head.chars().all(|c| "ivxlcdmIVXLCDM".contains(c));
    let width = open + label + 1;
    (enumerator && open <= 1 && tail[1..].starts_with(' ')).then_some(width + 1)
}

/// Lines that start a block of their own and are never joined to the line before.
fn starts_block(text: &str) -> bool {
    text.starts_with(".. ")
        || text == ".."
        || text.starts_with(':')
        || text.starts_with("| ")
        || text == "|"
        || text.starts_with('+')
        || text.starts_with(">>>")
        || (text.starts_with('-') && !text.starts_with("- ") && !text.starts_with("--- "))
        || list_marker(text).is_some()
}

/// A row or border of a grid table: kept as it is.
fn is_grid_line(text: &str) -> bool {
    text.len() > 1 && ((text.starts_with('+') && text.ends_with('+')) || (text.starts_with('|') && text.ends_with('|')))
}

/// A border of a simple table: `=====  ======`.
fn is_simple_border(text: &str) -> bool {
    text.contains("= ") && text.chars().all(|c| c == '=' || c == ' ')
}

/// The directive named on a line, e.g. `code-block` for `.. code-block:: python`.
fn directive_name(text: &str) -> Option<&str> {
    let rest = text.strip_prefix(".. ")?;
    let (name, _) = rest.split_once("::")?;
    let name = name.trim();
    (!name.is_empty() && !name.contains(' ')).then_some(name)
}

/// The text with paragraphs on one line and headings normalized.
pub fn reflow(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end()).collect();
    let mut out: Vec<String> = Vec::new();
    // The paragraph being collected: its first line's indentation, the
    // indentation its continuation lines have, and its text.
    let mut para: Option<(usize, usize, String)> = None;
    // Inside a literal block: lines indented deeper than this are kept as they are.
    let mut literal: Option<usize> = None;

    fn flush(out: &mut Vec<String>, para: &mut Option<(usize, usize, String)>) -> bool {
        match para.take() {
            Some((indent, _, text)) => {
                let literal_next = text.ends_with("::");
                out.push(format!("{}{text}", " ".repeat(indent)));
                literal_next
            }
            None => false,
        }
    }
    fn blank(out: &mut Vec<String>) {
        if out.last().is_some_and(|l| !l.is_empty()) {
            out.push(String::new());
        }
    }

    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let indent = indent_of(line);
        let text = line.trim_start();

        if let Some(base) = literal {
            if line.is_empty() || indent > base {
                if line.is_empty() {
                    blank(&mut out);
                } else {
                    out.push(line.to_string());
                }
                i += 1;
                continue;
            }
            literal = None;
        }

        if line.is_empty() {
            if flush(&mut out, &mut para) {
                literal = Some(indent_of(out.last().map(String::as_str).unwrap_or("")));
            }
            blank(&mut out);
            i += 1;
            continue;
        }

        // Section titles, at the top level: "Title" + "=====", or with an overline too.
        if indent == 0 && para.is_none() {
            let over = is_adornment(line) && lines.get(i + 2).is_some_and(|l| is_adornment(l));
            let (title_at, under_at) = if over { (i + 1, i + 2) } else { (i, i + 1) };
            let title = lines.get(title_at).map(|l| l.trim()).unwrap_or("");
            let under = lines.get(under_at).copied().unwrap_or("");
            // docutils wants the underline as long as the title; short ones only
            // pass from three characters on, so `--` under a word is no heading.
            let long_enough = under.trim_end().len() >= 3 || under.trim_end().len() >= title.chars().count();
            if !title.is_empty()
                && !is_adornment(title)
                && is_adornment(under)
                && long_enough
                && (!over || lines[i].trim_end().chars().next() == under.chars().next())
            {
                let mark = under.chars().next().expect("adornment is not empty");
                let rule: String = std::iter::repeat_n(mark, title.chars().count()).collect();
                if over {
                    out.push(rule.clone());
                }
                out.push(title.to_string());
                out.push(rule);
                i = under_at + 1;
                continue;
            }
            // A transition: a lone adornment line.
            if is_adornment(line) {
                out.push(line.to_string());
                i += 1;
                continue;
            }
        }

        // Directives: the line and its options as they are; literal content too.
        if let Some(name) = directive_name(text) {
            flush(&mut out, &mut para);
            out.push(line.to_string());
            i += 1;
            // Options follow right after the directive line.
            while i < lines.len() && !lines[i].is_empty() && indent_of(lines[i]) > indent {
                out.push(lines[i].to_string());
                i += 1;
            }
            if LITERAL_DIRECTIVES.contains(&name) {
                literal = Some(indent);
            }
            continue;
        }

        if is_grid_line(text) {
            flush(&mut out, &mut para);
            out.push(line.to_string());
            i += 1;
            continue;
        }
        // A simple table runs to a border followed by a blank line.
        if is_simple_border(text) {
            flush(&mut out, &mut para);
            out.push(line.to_string());
            i += 1;
            while i < lines.len() {
                out.push(lines[i].to_string());
                i += 1;
                if is_simple_border(lines[i - 1].trim_start()) && lines.get(i).is_none_or(|l| l.is_empty()) {
                    break;
                }
            }
            continue;
        }

        let joins = match &para {
            Some((_, cont, _)) => indent == *cont && !starts_block(text),
            None => false,
        };
        if joins {
            let (_, _, buf) = para.as_mut().expect("checked above");
            buf.push(' ');
            buf.push_str(text);
        } else {
            if flush(&mut out, &mut para) {
                literal = Some(indent_of(out.last().map(String::as_str).unwrap_or("")));
                // A literal block starts only after a blank line; here the text
                // goes on, so this line is part of it if it is indented.
                if indent > literal.unwrap_or(0) {
                    out.push(line.to_string());
                    i += 1;
                    continue;
                }
                literal = None;
            }
            // Field list items (`:status: open`) keep their own line; their
            // continuation lines are indented deeper.
            let cont = if text.starts_with(':') { usize::MAX } else { indent + list_marker(text).unwrap_or(0) };
            para = Some((indent, cont, text.to_string()));
            // Single-line blocks: line blocks and doctest lines.
            if text.starts_with('|') || text.starts_with(">>>") {
                flush(&mut out, &mut para);
            }
        }
        i += 1;
    }
    if flush(&mut out, &mut para) {
        // "::" at the very end: nothing follows.
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_paragraphs_and_items() {
        let a = "Intro that is\nwrapped here.\n\n- first item\n  goes on\n- second\n\n1. one\n   more\n";
        assert_eq!(reflow(a), "Intro that is wrapped here.\n\n- first item goes on\n- second\n\n1. one more\n");
        let b = "Intro that is wrapped\nhere.\n\n\n- first item goes\n  on\n- second   \n\n1. one\n   more";
        assert_eq!(reflow(a), reflow(b));
    }

    #[test]
    fn normalizes_headings() {
        let a = "Title\n=========\n\nText.\n\n=====\nPart\n=====\n";
        assert_eq!(reflow(a), "Title\n=====\n\nText.\n\n====\nPart\n====\n");
        // A transition is not a heading.
        assert_eq!(reflow("One.\n\n----\n\nTwo.\n"), "One.\n\n----\n\nTwo.\n");
    }

    #[test]
    fn a_lone_double_colon_is_no_heading() {
        let a = "Some text\nthat wraps.\n::\n\n    code line\n    more code\n";
        let once = reflow(a);
        assert_eq!(once, "Some text that wraps.\n::\n\n    code line\n    more code\n");
        assert_eq!(reflow(&once), once);
    }

    #[test]
    fn keeps_literal_blocks_tables_and_options() {
        let code = "Example::\n\n   a = 1\n   b = 2\n\nAfter\nthat.\n";
        assert_eq!(reflow(code), "Example::\n\n   a = 1\n   b = 2\n\nAfter that.\n");
        let directive = ".. code-block:: python\n   :linenos:\n\n   x = [\n     1,\n   ]\n\nText\nhere.\n";
        assert_eq!(
            reflow(directive),
            ".. code-block:: python\n   :linenos:\n\n   x = [\n     1,\n   ]\n\nText here.\n"
        );
        let table = "+---+---+\n| a | b |\n+---+---+\n";
        assert_eq!(reflow(table), table);
        let need = ".. req:: Title\n   :id: R_1\n   :status: open\n\n   Body that\n   wraps.\n";
        assert_eq!(reflow(need), ".. req:: Title\n   :id: R_1\n   :status: open\n\n   Body that wraps.\n");
        let simple = "=====  =====\nA      B\n=====  =====\nlong   row\nnext   row\n=====  =====\n\nText\nhere.\n";
        assert_eq!(
            reflow(simple),
            "=====  =====\nA      B\n=====  =====\nlong   row\nnext   row\n=====  =====\n\nText here.\n"
        );
        let fields = ":author: Me\n:date: today\n";
        assert_eq!(reflow(fields), fields);
    }
}
