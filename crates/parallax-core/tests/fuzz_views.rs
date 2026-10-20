//! Every content view and similar rule survives text that is not valid in its
//! language: unbalanced brackets, stray quotes, unfinished comments.

use parallax_core::convert;

struct Random(u64);

impl Random {
    fn next(&mut self) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as usize
    }
}

#[test]
fn views_survive_broken_source() {
    let pieces = [
        "let",
        "fn",
        "x",
        "=",
        "(",
        ")",
        "{",
        "}",
        "[",
        "]",
        "<",
        ">",
        "'",
        "\"",
        "`",
        "#",
        "//",
        "/*",
        "*/",
        "\\",
        "r#\"",
        "$",
        "@",
        ";",
        ",",
        ":",
        "::",
        ".",
        "->",
        "=>",
        "|",
        "&",
        "*",
        "use",
        "mod",
        "match",
        "def",
        "class",
        "sub",
        "my",
        "<#if",
        "${",
        "«",
        "»",
        "DEFINE",
        "ENDDEFINE",
        "\n",
        "\n    ",
        "\t",
        "0x",
        "1",
        "'a",
        "q{",
        "<<EOF",
        "EOF",
        "=pod",
        "=cut",
        "import",
        "export",
        "<div",
        "/>",
        "{%",
        "%}",
    ];
    let names = [
        "a.rs",
        "a.ts",
        "a.tsx",
        "a.js",
        "a.py",
        "a.pl",
        "a.pm",
        "a.xpt",
        "a.ext",
        "a.chk",
        "a.ftl",
        "a.rst",
        "a.toml",
        "a.yaml",
        "a.json",
        "a.csv",
        "conanfile.txt",
        "a.a2l",
        "a.dbc",
        "a.dcm",
        "a.odx-d",
        "Cargo.lock",
        "a.ipynb",
        "a.xml",
        "a.arxml",
    ];
    let mut random = Random(3);
    for round in 0..300 {
        let text = |random: &mut Random| {
            let n = 1 + random.next() % 120;
            (0..n).map(|_| pieces[random.next() % pieces.len()]).collect::<Vec<_>>().join(" ")
        };
        let (a, b) = (text(&mut random), text(&mut random));
        for name in names {
            for view in convert::views_for(name, a.as_bytes()) {
                let _ = convert::render(view, name, a.as_bytes());
            }
            let _ = convert::content_equal(name, a.as_bytes(), b.as_bytes());
            let _ = convert::content_equal(name, a.as_bytes(), a.as_bytes());
        }
        let _ = round;
    }
}

#[test]
fn automotive_descriptions_survive_damaged_exports() {
    let cases: [(&str, &[u8], convert::View); 2] = [
        ("engine.dcm", include_bytes!("fixtures/automotive/engine-v1.dcm"), convert::View::Calibration),
        ("engine.odx-d", include_bytes!("fixtures/automotive/engine-v1.odx-d"), convert::View::Diagnostics),
    ];
    let mut random = Random(91);
    for (name, original, view) in cases {
        for _ in 0..150 {
            let mut changed = original.to_vec();
            for _ in 0..1 + random.next() % 8 {
                // Keep the DCM signature, so the changed data still exercises its parser.
                let at = 40 + random.next() % (changed.len() - 40);
                changed[at] = b' ' + (random.next() % 95) as u8;
            }
            let _ = convert::render(view, name, &changed);
            let _ = convert::content_equal(name, original, &changed);
        }
        for end in (0..original.len()).step_by(37) {
            let _ = convert::render(view, name, &original[..end]);
        }
    }
}

#[test]
fn legacy_word_documents_render() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/legacy-office");
    for name in ["doc-report.doc", "doc-plain.doc", "doc-mixed.doc"] {
        let data = std::fs::read(format!("{dir}/{name}")).unwrap();
        // Read natively, so this does not depend on LibreOffice.
        let text = convert::render(convert::View::Document, name, &data).unwrap();
        assert!(text.lines().count() >= 3, "{name}: {text}");
        let _ = convert::content_equal(name, &data, &data);
    }
}

/// Binary Visio drawings with bytes changed inside their VisioDocument stream
/// (so that the compound file around it stays readable) render or fail, never
/// panic. Bytes before 0x40 stay: with a broken header the native reader gives
/// up, and the slow LibreOffice fallback would be tried.
#[test]
fn visio_drawings_survive_damage() {
    use std::io::{Read, Write};
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/visio");
    let mut random = Random(11);
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "vsd") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let data = std::fs::read(&path).unwrap();
        let mut stream = Vec::new();
        cfb::CompoundFile::open(std::io::Cursor::new(&data))
            .unwrap()
            .open_stream("/VisioDocument")
            .unwrap()
            .read_to_end(&mut stream)
            .unwrap();
        for view in convert::views_for(&name, &data) {
            let _ = convert::render(view, &name, &data);
        }
        for _ in 0..25 {
            let mut broken = stream.clone();
            for _ in 0..6 {
                let at = 0x40 + random.next() % (broken.len() - 0x40);
                broken[at] = random.next() as u8;
            }
            let mut file = cfb::CompoundFile::create(std::io::Cursor::new(Vec::new())).unwrap();
            file.create_stream("/VisioDocument").unwrap().write_all(&broken).unwrap();
            file.flush().unwrap();
            let broken = file.into_inner().into_inner();
            let _ = convert::render(convert::View::Diagram, &name, &broken);
            let _ = convert::content_equal(&name, &data, &broken);
        }
    }
}
