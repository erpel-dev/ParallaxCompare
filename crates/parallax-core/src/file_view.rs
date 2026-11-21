//! Which view two files open in, and their text in it: the original text, a
//! converted view (outline, PDF text, pretty JSON …), a hex dump or the image
//! compare. Files that are ≈ similar open in the view where they are equal, and
//! a view that cannot be made falls back to text or hex, with a note saying why.
//!
//! Shared by the desktop app's `open_file_diff` command and the GPUI front end,
//! so both open a file the same way.

use crate::convert::{self, ImageDiff, View};
use crate::source::Source;

/// Summary views of files larger than this read only headers (models, media):
/// such files are not compared byte for byte unless asked.
pub const FULL_COMPARE_LIMIT: usize = 64 << 20;

/// The view chosen for two files, and what it shows.
#[derive(Debug)]
pub struct Chosen {
    pub view: View,
    /// The views both files offer, in the order to offer them.
    pub views: Vec<View>,
    /// Why the files are shown the way they are (≈ similar, a fallback …).
    pub note: Option<String>,
    /// For files asked to be shown in a particular view: the view where they are equal.
    pub equal_view: Option<View>,
    pub identical: bool,
    /// The files are large and the view reads only headers, so their bytes were not compared.
    pub unread: bool,
    pub image: Option<ImageDiff>,
    /// Per side, for views other than text and the image compare: the view's text,
    /// or why it could not be made. `None` where the side has no file.
    pub texts: [Option<Result<String, String>>; 2],
}

/// Chooses the view for the file at `path` (`name` picks views and grammars) with
/// `data` on each side, `requested` if the user picked one. `binary` tells which
/// fallback a failed view gets: hex for binary files, else text.
pub fn choose(
    sources: [&Source; 2],
    path: &str,
    name: &str,
    data: [Option<&[u8]>; 2],
    binary: bool,
    requested: Option<View>,
    schemas: [Option<&str>; 2],
) -> Chosen {
    let offered =
        |side: usize| data[side].map(|d| convert::views_for_source(sources[side], path, name, d, schemas[side]));
    let views = match (offered(0), offered(1)) {
        (Some(l), Some(r)) => l.into_iter().filter(|v| r.contains(v)).collect::<Vec<_>>(),
        (Some(v), None) | (None, Some(v)) => v,
        _ => vec![View::Text],
    };
    let mut chosen = requested.filter(|v| views.contains(v)).unwrap_or(views[0]);
    // Summary views of large files (models, media) read only headers: comparing all
    // bytes would read gigabytes of weights just to say whether the files are equal.
    let unread = matches!(data, [Some(l), Some(r)] if l.len() == r.len())
        && data[0].map_or(0, <[u8]>::len) > FULL_COMPARE_LIMIT
        && !matches!(chosen, View::Text | View::Hex | View::Image);
    let identical = chosen != View::FlatbufferData && !unread && data[0].is_some() && data[0] == data[1];
    let mut note = None;
    // The texts of the chosen view, when finding it already made them.
    let mut rendered = None;
    // ≈ similar files open in the view where they are equal, and say why. Shown in
    // their original bytes, the view where they are equal is offered instead.
    let mut equal_view = None;
    let similar = match data {
        [Some(l), Some(r)] if !identical && (requested.is_none() || matches!(chosen, View::Text | View::Hex)) => {
            convert::similar_view(name, l, r, &views)
        }
        _ => None,
    };
    let similar = match similar {
        Some(found) if requested.is_some() => {
            equal_view = Some(found.view);
            None
        }
        other => other,
    };
    if let Some(convert::Similar { view: similar, reason, texts }) = similar {
        chosen = similar;
        // Flatbuffer data needs the schemas the generic rendering does not have.
        rendered = texts.filter(|_| similar != View::FlatbufferData);
        note = Some(if similar == View::Image {
            let meta = if views.contains(&View::ImageMetadata) { " “Image metadata” shows how." } else { "" };
            format!("≈ Similar: {reason}. The pixels are identical; the files differ in metadata or encoding.{meta}")
        } else {
            format!(
                "≈ Similar: {reason}. Shown as “{}”, where the files are equal; “Text” shows every difference in the bytes.",
                similar.label()
            )
        });
    }

    let mut image = None;
    if chosen == View::Image {
        match convert::image_diff(name, data[0], data[1]) {
            Ok(d) => {
                note = d.note.clone().or(note);
                image = Some(d);
            }
            Err(e) => {
                note = Some(format!("The image could not be decoded ({e}); showing a hex dump."));
                chosen = View::Hex;
            }
        }
    }

    // Converted text for both sides; a slow converter on one side need not hold up
    // the other, so the two sides run at the same time. A converter that panics on
    // damaged input fails like one that returns an error, on either side.
    let render = |view: View, side: usize| -> Option<Result<String, String>> {
        data[side].map(|d| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                convert::render_source(view, sources[side], path, name, d, schemas[side]).map_err(|e| e.to_string())
            }))
            .unwrap_or_else(|_| Err("the file could not be read (internal error)".into()))
        })
    };
    let both = |view: View| {
        std::thread::scope(|s| {
            let l = s.spawn(|| render(view, 0));
            let r = render(view, 1);
            (l.join().unwrap_or_else(|_| Some(Err("the file could not be read (internal error)".into()))), r)
        })
    };
    let (mut left_text, mut right_text) = (None, None);
    if let Some((l, r)) = rendered.filter(|_| !matches!(chosen, View::Image | View::Text)) {
        (left_text, right_text) = (Some(Ok(l)), Some(Ok(r)));
    } else if !matches!(chosen, View::Image | View::Text) {
        (left_text, right_text) = both(chosen);
        // Either side failing falls back, whichever it is.
        let failed = [&left_text, &right_text].into_iter().flatten().find_map(|r| r.as_ref().err()).cloned();
        if let Some(e) = failed {
            let fallback = if binary { View::Hex } else { View::Text };
            note = Some(format!("{} is not available: {e}. Showing {}.", chosen.label(), fallback.label()));
            chosen = fallback;
            (left_text, right_text) =
                if fallback == View::Hex { (render(fallback, 0), render(fallback, 1)) } else { (None, None) };
        }
    }

    // Equal simplified values do not imply equal AUTOSAR content. Show omitted
    // differences by default, and explain the limitation for an explicitly chosen view.
    if chosen == View::EcucValues
        && !identical
        && matches!((&left_text, &right_text), (Some(Ok(l)), Some(Ok(r))) if l == r)
    {
        let (l, r) = both(View::ArxmlStructure);
        match (l, r) {
            (Some(Ok(l)), Some(Ok(r))) if l != r => {
                note = Some("ECUC values match, but other AUTOSAR content differs. AUTOSAR structure shows full definitions, reference types, attributes and metadata.".into());
                if requested.is_none() {
                    chosen = View::ArxmlStructure;
                    (left_text, right_text) = (Some(Ok(l)), Some(Ok(r)));
                }
            }
            // An incomplete full-tree comparison must not imply that the files match.
            (Some(Err(e)), _) | (_, Some(Err(e))) => {
                note = Some(format!(
                    "ECUC values match; full AUTOSAR content could not be checked ({e}). Use Text to inspect all differences."
                ));
            }
            _ => {}
        }
    }

    Chosen { view: chosen, views, note, equal_view, identical, unread, image, texts: [left_text, right_text] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{SourceOptions, SourceSpec, open_pair};

    fn pair(dir: &std::path::Path, name: &str, left: &[u8], right: &[u8]) -> (Source, Source) {
        let (l, r) = (dir.join("l"), dir.join("r"));
        std::fs::create_dir_all(&l).unwrap();
        std::fs::create_dir_all(&r).unwrap();
        std::fs::write(l.join(name), left).unwrap();
        std::fs::write(r.join(name), right).unwrap();
        let spec = |p: &std::path::Path| SourceSpec::Local { path: p.to_string_lossy().into_owned() };
        open_pair(&spec(&l), &spec(&r), SourceOptions::default(), false).unwrap()
    }

    #[test]
    fn json_differing_only_in_layout_opens_in_the_view_where_it_is_equal() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = (br#"{"a":1,"b":[1,2]}"#.as_slice(), b"{\n  \"a\": 1,\n  \"b\": [1, 2]\n}\n".as_slice());
        let (l, r) = pair(dir.path(), "x.json", left, right);
        let chosen = choose([&l, &r], "x.json", "x.json", [Some(left), Some(right)], false, None, [None, None]);
        assert_ne!(chosen.view, View::Text);
        assert!(chosen.views.contains(&View::Text));
        assert!(chosen.note.as_deref().is_some_and(|n| n.starts_with("≈ Similar")), "{:?}", chosen.note);
        let [Some(Ok(a)), Some(Ok(b))] = &chosen.texts else { panic!("both sides rendered: {:?}", chosen.texts) };
        assert_eq!(a, b);
        // Asked for text, the files show as they are, and the equal view is offered.
        let text =
            choose([&l, &r], "x.json", "x.json", [Some(left), Some(right)], false, Some(View::Text), [None, None]);
        assert_eq!(text.view, View::Text);
        assert_eq!(text.equal_view, Some(chosen.view));
        assert!(matches!(text.texts, [None, None]));
    }

    #[test]
    fn identical_and_missing_sides() {
        let dir = tempfile::tempdir().unwrap();
        let (l, r) = pair(dir.path(), "a.txt", b"same\n", b"same\n");
        let same = choose([&l, &r], "a.txt", "a.txt", [Some(b"same\n"), Some(b"same\n")], false, None, [None, None]);
        assert!(same.identical);
        assert_eq!(same.view, View::Text);
        let one = choose([&l, &r], "a.txt", "a.txt", [Some(b"same\n"), None], false, None, [None, None]);
        assert!(!one.identical);
        assert_eq!(one.view, View::Text);
    }

    #[test]
    fn binary_files_get_a_hex_dump() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = (b"\x00\x01\x02binary".as_slice(), b"\x00\x01\x03binary".as_slice());
        let (l, r) = pair(dir.path(), "blob.bin", left, right);
        let chosen = choose([&l, &r], "blob.bin", "blob.bin", [Some(left), Some(right)], true, None, [None, None]);
        assert_eq!(chosen.view, View::Hex);
        assert!(matches!(&chosen.texts, [Some(Ok(a)), Some(Ok(_))] if a.contains("00 01 02")), "{:?}", chosen.texts);
    }
}
