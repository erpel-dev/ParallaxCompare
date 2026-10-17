//! Re-indenting XML and JSON so that minified files diff line by line.

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;

use crate::{Error, Result};

pub fn xml(text: &str) -> Result<String> {
    // Only whitespace between elements is dropped; text keeps its spaces, also
    // around entity references (`trim_text` would turn `a &amp; b` into `a&amp;b`).
    let mut reader = Reader::from_str(text);
    let mut writer = Writer::new_with_indent(Vec::with_capacity(text.len() + text.len() / 4), b' ', 2);
    loop {
        match reader.read_event().map_err(|e| Error::Unsupported(format!("XML: {e}")))? {
            Event::Eof => break,
            Event::Text(t) if t.trim_ascii().is_empty() => {}
            event => writer.write_event(event).map_err(|e| Error::Unsupported(format!("XML: {e}")))?,
        }
    }
    let mut out = String::from_utf8(writer.into_inner()).map_err(|e| Error::Unsupported(e.to_string()))?;
    out.push('\n');
    Ok(out)
}

pub fn json(data: &[u8]) -> Result<String> {
    let text = crate::text::decode(data).text;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| Error::Unsupported(format!("JSON: {e}")))?;
    let mut out = serde_json::to_string_pretty(&value).map_err(|e| Error::Unsupported(e.to_string()))?;
    out.push('\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn indents() {
        let x = super::xml("<a><b x=\"1\">t &amp; u</b><c/></a>").unwrap();
        assert_eq!(x, "<a>\n  <b x=\"1\">t &amp; u</b>\n  <c/>\n</a>\n");
        let j = super::json(br#"{"z":1,"a":[1,2]}"#).unwrap();
        assert_eq!(j, "{\n  \"z\": 1,\n  \"a\": [\n    1,\n    2\n  ]\n}\n");
    }
}
