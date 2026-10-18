//! Apache FreeMarker templates (`.ftl`, `.ftlh`, `.ftlx`).
//!
//! The outline keeps the template's logic and drops the markup around it: each
//! directive (`<#if …>`, `<#list … as x>`), user-defined directive call
//! (`<@layout.page …>`) and interpolation (`${…}`) on a line of its own,
//! indented by nesting. Comments go, and `<#else>` and its kin sit level with
//! the directive they belong to. Both tag syntaxes, `<#…>` and `[#…]`, are read.
//!
//! Templates differing only in `<#-- … -->` comments are similar: comments never
//! reach the output.

/// Whether `.ftl` content is a FreeMarker template; Mozilla Fluent uses the
/// extension too.
pub fn is_template(data: &[u8]) -> bool {
    let has = |needle: &[u8]| data.windows(needle.len()).any(|w| w == needle);
    has(b"<#") || has(b"</#") || has(b"<@") || has(b"[#") || has(b"${")
}

/// A piece of template syntax found in the text.
#[derive(Debug, PartialEq)]
enum Token {
    /// `<#name …>` or `<@name …>` (`user` set), with the tag as written.
    Open {
        name: String,
        user: bool,
        closed: bool,
        text: String,
    },
    /// `</#name>`, `</@name>` or `</@>` (empty name).
    Close {
        name: String,
        user: bool,
    },
    Interpolation(String),
}

/// Directives that continue the enclosing block instead of nesting.
const BRANCHES: &[&str] = &["else", "elseif", "recover"];

/// The end of a tag or interpolation that opens at `from`: the byte after the
/// closing `close`, skipping strings and, for tags, `>` inside parentheses.
fn scan_end(s: &str, from: usize, close: u8) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0usize;
    let mut i = from;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') => {
                let raw = i > 0 && b[i - 1] == b'r';
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' && !raw {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' if depth > 0 => depth -= 1,
            c if c == close && depth == 0 => return Some(i + 1),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Runs of whitespace as one space, outside of string literals.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut quote: Option<char> = None;
    let mut space = false;
    let mut escaped = false;
    for c in s.chars() {
        if let Some(q) = quote {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        if c == '"' || c == '\'' {
            quote = Some(c);
        }
        out.push(c);
    }
    out
}

fn tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let rest = &text[i..];
        let square = rest.starts_with('[');
        let (open, close) = if square { ('[', b']') } else { ('<', b'>') };
        if rest.starts_with("<#--") || rest.starts_with("[#--") {
            let end = if square { "--]" } else { "-->" };
            i = rest[4..].find(end).map_or(b.len(), |n| i + 4 + n + 3);
            continue;
        }
        if (rest.starts_with("${") || rest.starts_with("#{"))
            && let Some(end) = scan_end(text, i + 2, b'}')
        {
            out.push(Token::Interpolation(format!("{}{{{}}}", &rest[..1], collapse(&text[i + 2..end - 1]))));
            i = end;
            continue;
        }
        let after = rest.get(1..).unwrap_or_default();
        if rest.starts_with(open) && (after.starts_with('#') || after.starts_with('@')) {
            let user = after.starts_with('@');
            let name: String = after[1..].chars().take_while(|c| c.is_alphanumeric() || "_$.:-".contains(*c)).collect();
            if let Some(end) = scan_end(text, i + 2 + name.len(), close).filter(|_| user || !name.is_empty()) {
                let tag = collapse(&text[i..end]);
                let closed = tag.ends_with(&format!("/{}", close as char));
                // The text of noparse is not template syntax.
                if name == "noparse" {
                    let stop = if square { "[/#noparse]" } else { "</#noparse>" };
                    out.push(Token::Open { name, user, closed: true, text: tag });
                    i = text[end..].find(stop).map_or(b.len(), |n| end + n + stop.len());
                    continue;
                }
                out.push(Token::Open { name, user, closed, text: tag });
                i = end;
                continue;
            }
        }
        if rest.starts_with(&format!("{open}/#")) || rest.starts_with(&format!("{open}/@")) {
            let user = rest.as_bytes()[2] == b'@';
            let name: String = rest[3..].chars().take_while(|c| c.is_alphanumeric() || "_$.:-".contains(*c)).collect();
            let head = 3 + name.len();
            if let Some(n) = rest[head..].find(close as char) {
                out.push(Token::Close { name, user });
                i += head + n + 1;
                continue;
            }
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// The template's directives, calls and interpolations, indented by nesting.
pub fn outline(text: &str) -> String {
    let tokens = tokens(text);
    // Which opening tags have a closing tag: those are blocks.
    let mut block = vec![false; tokens.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        match t {
            Token::Open { name, closed: false, .. } if !BRANCHES.contains(&name.as_str()) => stack.push(i),
            Token::Close { name, user } => {
                let matches = |j: &usize| match &tokens[*j] {
                    Token::Open { name: n, user: u, .. } => u == user && (n == name || (*user && name.is_empty())),
                    _ => false,
                };
                if let Some(at) = stack.iter().rposition(matches) {
                    block[stack[at]] = true;
                    stack.truncate(at);
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    let mut depth = 0usize;
    let mut line = |depth: usize, s: &str| {
        out.push_str(&"  ".repeat(depth));
        out.push_str(s);
        out.push('\n');
    };
    for (i, t) in tokens.iter().enumerate() {
        match t {
            Token::Open { name, text, .. } if BRANCHES.contains(&name.as_str()) => line(depth.saturating_sub(1), text),
            Token::Open { text, .. } => {
                line(depth, text);
                if block[i] {
                    depth += 1;
                }
            }
            Token::Close { .. } => depth = depth.saturating_sub(1),
            Token::Interpolation(s) => line(depth, s),
        }
    }
    out
}

/// The template without FreeMarker comments; a line that held nothing but a
/// comment goes with it.
pub fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = [rest.find("<#--"), rest.find("[#--")].into_iter().flatten().min() {
        let end = if rest[at..].starts_with('[') { "--]" } else { "-->" };
        let Some(n) = rest[at + 4..].find(end) else { break };
        out.push_str(&rest[..at]);
        rest = &rest[at + 4 + n + 3..];
        // A comment alone on its line takes the line with it.
        let line_start = out.rfind('\n').map_or(0, |p| p + 1);
        let line_end = rest.find('\n').unwrap_or(rest.len());
        if out[line_start..].trim().is_empty() && rest[..line_end].trim().is_empty() {
            out.truncate(line_start);
            rest = &rest[(line_end + 1).min(rest.len())..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_freemarker_from_fluent() {
        assert!(is_template(b"<#if x>y</#if>"));
        assert!(is_template(b"Hello ${name}!"));
        assert!(!is_template(b"hello = Hello, { $name }!\n"));
    }

    #[test]
    fn outlines_the_template_logic() {
        let text = r#"<#import "/lib/ui.ftl" as ui>
<#-- the page -->
<#macro page   title>
  <html><title>${title?html}</title>
  <#if (items?size > 0)>
    <ul>
    <#list items as item>
      <li class="${item.kind}">${item.name}</li>
    <#else>
      <li>none</li>
    </#list>
    </ul>
  <#elseif user??>
    <@ui.card title="Hi ${user}"/>
  </#if>
  <@ui.box><#nested></@>
</#macro>
<#noparse>${raw} <#if></#noparse>
"#;
        assert_eq!(
            outline(text),
            r#"<#import "/lib/ui.ftl" as ui>
<#macro page title>
  ${title?html}
  <#if (items?size > 0)>
    <#list items as item>
      ${item.kind}
      ${item.name}
    <#else>
  <#elseif user??>
    <@ui.card title="Hi ${user}"/>
  <@ui.box>
    <#nested>
<#noparse>
"#
        );
        assert_eq!(outline("<td>${n} × ${p}</td>…<#if ä>ö</#if>"), "${n}\n${p}\n<#if ä>\n");
        let square = "[#list xs as x]${x}[/#list]";
        assert_eq!(outline(square), "[#list xs as x]\n  ${x}\n");
    }

    #[test]
    fn strips_comments() {
        let a = "<p>\n  <#-- why -->\n  ${x} <#-- inline --> y\n</p>\n";
        assert_eq!(without_comments(a), "<p>\n  ${x}  y\n</p>\n");
        assert_eq!(without_comments("a[#-- c --]b"), "ab");
        assert_eq!(without_comments("<#-- open"), "<#-- open");
    }
}
